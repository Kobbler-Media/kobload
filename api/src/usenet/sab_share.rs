use std::collections::HashMap;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use serde::Deserialize;
use sqlx::PgPool;
use tracing::warn;
use uuid::Uuid;

use crate::repositories::clients;
use crate::types::DownloadClientConfig;

const TIMEOUT: Duration = Duration::from_secs(5);

type LastSeen = HashMap<(Uuid, String), Vec<SabServer>>;

// A SAB whose API stops answering may still hold its connections, so the engine must not
// grow back into its share until SAB is disabled.
static LAST_SEEN: LazyLock<Mutex<LastSeen>> = LazyLock::new(Default::default);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SabServer {
    pub host: String,
    pub username: String,
    pub connections: i64,
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

fn number(value: &serde_json::Value) -> Option<i64> {
    match value {
        serde_json::Value::Number(n) => n.as_i64(),
        serde_json::Value::String(s) => s.trim().parse().ok(),
        serde_json::Value::Bool(b) => Some(i64::from(*b)),
        _ => None,
    }
}

// An unreachable SAB never blocks an edit: it may be the very thing being fixed.
pub async fn sab_servers(pool: &PgPool, http: &reqwest::Client) -> Vec<SabServer> {
    enabled_sab_servers(pool, http, None).await
}

pub async fn enabled_sab_servers(
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
    let mut servers = Vec::new();
    for sab in sabs
        .iter()
        .filter(|c| c.enabled && c.client_type == "sabnzbd" && Some(c.id) != except)
    {
        servers.extend(client_servers(http, sab).await);
    }
    servers
}

pub async fn client_servers(http: &reqwest::Client, sab: &DownloadClientConfig) -> Vec<SabServer> {
    let key = (sab.id, sab.base_url.clone());
    let seen = || LAST_SEEN.lock().unwrap_or_else(|e| e.into_inner());
    match fetch(http, sab).await {
        Ok(found) => {
            seen().insert(key, found.clone());
            found
        }
        Err(reason) => {
            let last = seen().get(&key).cloned().unwrap_or_default();
            warn!(
                client = %sab.name,
                reason,
                remembered_servers = last.len(),
                "Could not read SABnzbd's servers; the usenet cap check uses its last known share"
            );
            last
        }
    }
}

async fn fetch(
    http: &reqwest::Client,
    sab: &DownloadClientConfig,
) -> Result<Vec<SabServer>, String> {
    let api_key = sab.api_key.as_deref().unwrap_or_default();
    let resp = http
        .get(format!("{}/api", sab.base_url.trim_end_matches('/')))
        .query(&[
            ("mode", "get_config"),
            ("section", "servers"),
            ("output", "json"),
            ("apikey", api_key),
        ])
        .timeout(TIMEOUT)
        .send()
        .await
        .map_err(|e| e.without_url().to_string())?;
    if !resp.status().is_success() {
        return Err(format!("SABnzbd answered {}", resp.status()));
    }
    let reply: ConfigReply = resp.json().await.map_err(|e| e.without_url().to_string())?;
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
