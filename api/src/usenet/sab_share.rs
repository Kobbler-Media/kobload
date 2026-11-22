use std::time::Duration;

use serde::Deserialize;
use sqlx::PgPool;
use tracing::warn;

use crate::repositories::clients;
use crate::types::DownloadClientConfig;

const TIMEOUT: Duration = Duration::from_secs(5);

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

/// SAB's per-server connections on the same accounts, so the engine's plus SAB's stay
/// within each provider cap. Only host, username and connection count are read. An
/// unreachable SAB counts as nothing rather than blocking edits: it may be the very thing
/// being fixed, and its connections are logged as unknown.
pub async fn sab_servers(pool: &PgPool, http: &reqwest::Client) -> Vec<SabServer> {
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
        .filter(|c| c.enabled && c.client_type == "sabnzbd")
    {
        match fetch(http, sab).await {
            Ok(found) => servers.extend(found),
            Err(reason) => warn!(
                client = %sab.name,
                reason,
                "Could not read SABnzbd's servers; its connections are left out of the usenet cap check"
            ),
        }
    }
    servers
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
