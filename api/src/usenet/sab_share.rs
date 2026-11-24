use std::time::Duration;

use futures::future::join_all;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use time::OffsetDateTime;
use tracing::warn;
use uuid::Uuid;

use crate::repositories::{app_settings, clients, usenet_servers};
use crate::types::DownloadClientConfig;

const TIMEOUT: Duration = Duration::from_secs(5);
const SETTING_PREFIX: &str = "usenet_sab_share:";
const DRAIN_REFRESH_SECS: i64 = 300;
// A disabled SAB that died mid-queue must not keep the engine shrunk for good.
const DRAIN_TRUSTED_SECS: i64 = 3600;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SabServer {
    pub host: String,
    pub username: String,
    pub connections: i64,
}

// Persisted because a SAB whose API is down may still hold its connections, and a kobload
// restart must not hand them to the engine as well.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
struct StoredShare {
    base_url: String,
    servers: Vec<SabServer>,
    draining: bool,
    seen_at: i64,
}

#[derive(Deserialize)]
struct ConfigReply {
    config: Servers,
}

#[derive(Deserialize)]
struct Servers {
    #[serde(default)]
    servers: Vec<ServerEntry>,
}

#[derive(Deserialize)]
struct ServerEntry {
    #[serde(default)]
    host: String,
    #[serde(default)]
    username: String,
    #[serde(default)]
    connections: serde_json::Value,
    #[serde(default)]
    enable: serde_json::Value,
}

#[derive(Deserialize)]
struct QueueReply {
    queue: Queue,
}

#[derive(Deserialize)]
struct Queue {
    #[serde(default)]
    paused: bool,
    #[serde(default)]
    slots: Vec<serde_json::Value>,
}

impl Queue {
    fn working(&self) -> bool {
        self.slots
            .iter()
            .any(|slot| slot_working(slot, self.paused))
    }
}

// Mirrors SABnzbd's own `has_forced_jobs` and `is_empty`: Force jobs download through a global
// pause, and a Grabbing job is still fetching its NZB over HTTP. An unknown status counts as
// working so that a new SAB version errs towards the account cap.
fn slot_working(slot: &serde_json::Value, queue_paused: bool) -> bool {
    let status = slot["status"].as_str().unwrap_or_default();
    !["Paused", "Grabbing", "Deleted"].contains(&status)
        && (!queue_paused || slot["priority"].as_str() == Some("Force"))
}

fn number(value: &serde_json::Value) -> Option<i64> {
    match value {
        serde_json::Value::Number(n) => n.as_i64(),
        serde_json::Value::String(s) => s.trim().parse().ok(),
        serde_json::Value::Bool(b) => Some(i64::from(*b)),
        _ => None,
    }
}

fn setting(id: Uuid) -> String {
    format!("{SETTING_PREFIX}{id}")
}

pub async fn sab_servers(pool: &PgPool, http: &reqwest::Client) -> Vec<SabServer> {
    sab_servers_except(pool, http, None).await
}

// Disabled clients are included: they keep finishing their queue, and hold connections
// until it is empty.
pub async fn sab_servers_except(
    pool: &PgPool,
    http: &reqwest::Client,
    except: Option<Uuid>,
) -> Vec<SabServer> {
    let sabs = match clients::list_clients(pool).await {
        Ok(cs) => cs,
        Err(e) => {
            warn!(error = %e, "Could not list download clients for the usenet cap check");
            return Vec::new();
        }
    };
    let mut servers: Vec<SabServer> = join_all(
        sabs.iter()
            .filter(|c| c.client_type == "sabnzbd" && Some(c.id) != except)
            .map(|sab| share(pool, http, sab)),
    )
    .await
    .into_iter()
    .flatten()
    .collect();
    servers.extend(deleted_shares(pool, &sabs, except).await);
    servers
}

// A SAB deleted while unreachable may still be downloading, so its share outlives the row
// until the trust period ends or the same SAB is added back and counts for itself.
async fn deleted_shares(
    pool: &PgPool,
    clients: &[DownloadClientConfig],
    except: Option<Uuid>,
) -> Vec<SabServer> {
    let stored = match app_settings::list_by_prefix(pool, SETTING_PREFIX).await {
        Ok(stored) => stored,
        Err(e) => {
            warn!(error = %e, "Could not load the shares of deleted SABnzbd clients");
            return Vec::new();
        }
    };
    let now = OffsetDateTime::now_utc().unix_timestamp();
    let mut held = Vec::new();
    for (key, value) in stored {
        let Some(id) = key
            .strip_prefix(SETTING_PREFIX)
            .and_then(|id| Uuid::parse_str(id).ok())
        else {
            continue;
        };
        if Some(id) == except || clients.iter().any(|c| c.id == id) {
            continue;
        }
        let share = serde_json::from_value::<StoredShare>(value)
            .ok()
            .filter(|s| s.draining && now - s.seen_at < DRAIN_TRUSTED_SECS);
        match share {
            Some(share) => {
                let re_added = clients
                    .iter()
                    .any(|c| c.client_type == "sabnzbd" && c.base_url == share.base_url);
                if !re_added {
                    held.extend(share.servers);
                }
            }
            None => forget_sab_share(pool, id).await,
        }
    }
    with_logins(pool, held).await
}

// The trust period starts now for a client that was counting in full; one already draining
// keeps the clock it had, so repeated edits cannot extend it.
pub async fn hold_sab_share(pool: &PgPool, sab: &DownloadClientConfig, was_enabled: bool) {
    let Some(mut stored) = load(pool, sab).await else {
        return;
    };
    if was_enabled {
        stored.draining = true;
        stored.seen_at = OffsetDateTime::now_utc().unix_timestamp();
    } else if !stored.draining {
        forget_sab_share(pool, sab.id).await;
        return;
    }
    save(pool, sab.id, &stored).await;
}

pub async fn hold_sab_share_if_unreachable(
    pool: &PgPool,
    http: &reqwest::Client,
    sab: &DownloadClientConfig,
) {
    if let Err(reason) = queue_working(http, sab).await {
        warn!(client = %sab.name, reason, "Disabled a SABnzbd that is not answering; keeping its share");
        hold_sab_share(pool, sab, true).await;
    }
}

async fn share(
    pool: &PgPool,
    http: &reqwest::Client,
    sab: &DownloadClientConfig,
) -> Vec<SabServer> {
    let stored = load(pool, sab).await;
    let now = OffsetDateTime::now_utc().unix_timestamp();
    match read(http, sab).await {
        Ok((servers, draining)) => {
            let current = StoredShare {
                base_url: sab.base_url.clone(),
                servers: without_logins(&servers),
                draining,
                seen_at: now,
            };
            if needs_saving(stored.as_ref(), &current) {
                save(pool, sab.id, &current).await;
            }
            if sab.enabled || draining {
                servers
            } else {
                Vec::new()
            }
        }
        Err(reason) => {
            let last = stored
                .filter(|s| sab.enabled || (s.draining && now - s.seen_at < DRAIN_TRUSTED_SECS))
                .map(|s| s.servers)
                .unwrap_or_default();
            let last = with_logins(pool, last).await;
            warn!(
                client = %sab.name,
                reason,
                remembered_servers = last.len(),
                "Could not read SABnzbd; the usenet cap check uses its last known share"
            );
            last
        }
    }
}

fn login_digest(username: &str) -> String {
    const_hex::encode(Sha256::digest(username.trim()))
}

// Settings land in backups, so the share keeps a digest that can only be matched back to a
// login the usenet server settings already hold.
fn without_logins(servers: &[SabServer]) -> Vec<SabServer> {
    servers
        .iter()
        .map(|s| SabServer {
            username: login_digest(&s.username),
            ..s.clone()
        })
        .collect()
}

async fn with_logins(pool: &PgPool, mut servers: Vec<SabServer>) -> Vec<SabServer> {
    let rows = match usenet_servers::list(pool).await {
        Ok(rows) => rows,
        Err(e) => {
            warn!(error = %e, "Could not match SABnzbd's last known share to its accounts");
            return servers;
        }
    };
    for server in &mut servers {
        if let Some(row) = rows
            .iter()
            .find(|r| login_digest(&r.username) == server.username)
        {
            server.username.clone_from(&row.username);
        }
    }
    servers
}

fn needs_saving(stored: Option<&StoredShare>, current: &StoredShare) -> bool {
    stored.is_none_or(|s| {
        s.servers != current.servers
            || s.draining != current.draining
            || (current.draining && current.seen_at - s.seen_at >= DRAIN_REFRESH_SECS)
    })
}

async fn load(pool: &PgPool, sab: &DownloadClientConfig) -> Option<StoredShare> {
    let value = match app_settings::get(pool, &setting(sab.id)).await {
        Ok(value) => value?,
        Err(e) => {
            warn!(error = %e, "Could not load SABnzbd's last known share");
            return None;
        }
    };
    serde_json::from_value::<StoredShare>(value)
        .ok()
        .filter(|s| s.base_url == sab.base_url)
}

async fn save(pool: &PgPool, id: Uuid, share: &StoredShare) {
    let Ok(value) = serde_json::to_value(share) else {
        return;
    };
    if let Err(e) = app_settings::set(pool, &setting(id), &value).await {
        warn!(error = %e, "Could not store SABnzbd's last known share");
    }
}

pub async fn forget_sab_share(pool: &PgPool, id: Uuid) {
    if let Err(e) = app_settings::delete(pool, &setting(id)).await {
        warn!(error = %e, "Could not drop SABnzbd's last known share");
    }
}

// An enabled client counts whatever its queue says, so only a disabled one needs the answer.
async fn read(
    http: &reqwest::Client,
    sab: &DownloadClientConfig,
) -> Result<(Vec<SabServer>, bool), String> {
    let (servers, queue) = tokio::join!(fetch(http, sab), queue_working(http, sab));
    // An unread queue must not be stored as idle: if this SAB is disabled while still down,
    // that stored answer is what decides whether it keeps its connections.
    let draining = if sab.enabled {
        queue.unwrap_or(true)
    } else {
        queue?
    };
    Ok((servers?, draining))
}

pub async fn candidate_servers(
    http: &reqwest::Client,
    sab: &DownloadClientConfig,
) -> Vec<SabServer> {
    fetch(http, sab).await.unwrap_or_else(|reason| {
        warn!(client = %sab.name, reason, "Could not read SABnzbd's servers for the cap check");
        Vec::new()
    })
}

async fn api(
    http: &reqwest::Client,
    sab: &DownloadClientConfig,
    query: &[(&str, &str)],
) -> Result<reqwest::Response, String> {
    let api_key = sab.api_key.as_deref().unwrap_or_default();
    let resp = http
        .get(format!("{}/api", sab.base_url.trim_end_matches('/')))
        .query(query)
        .query(&[("output", "json"), ("apikey", api_key)])
        .timeout(TIMEOUT)
        .send()
        .await
        .map_err(|e| e.without_url().to_string())?;
    if !resp.status().is_success() {
        return Err(format!("SABnzbd answered {}", resp.status()));
    }
    Ok(resp)
}

pub async fn queue_working(
    http: &reqwest::Client,
    sab: &DownloadClientConfig,
) -> Result<bool, String> {
    let reply: QueueReply = api(http, sab, &[("mode", "queue")])
        .await?
        .json()
        .await
        .map_err(|e| e.without_url().to_string())?;
    Ok(reply.queue.working())
}

async fn fetch(
    http: &reqwest::Client,
    sab: &DownloadClientConfig,
) -> Result<Vec<SabServer>, String> {
    let reply: ConfigReply = api(http, sab, &[("mode", "get_config"), ("section", "servers")])
        .await?
        .json()
        .await
        .map_err(|e| e.without_url().to_string())?;
    Ok(reply
        .config
        .servers
        .into_iter()
        .filter(|s| number(&s.enable).unwrap_or(1) != 0)
        .map(|s| SabServer {
            host: s.host,
            username: s.username,
            connections: number(&s.connections).unwrap_or(0).max(0),
        })
        .collect())
}
