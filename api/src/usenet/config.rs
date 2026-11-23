use sqlx::PgPool;
use time::Date;
use time::macros::format_description;
use tracing::warn;
use usenet_proto::{EngineConfigResponse, IpPreference, UsenetServer};

use super::caps;
use super::sab_share::sab_servers;
use crate::repositories::usenet_servers::UsenetServerRow;
use crate::repositories::{RepositoryError, usenet_servers};

pub fn format_date(date: Date) -> String {
    date.format(format_description!("[year]-[month]-[day]"))
        .unwrap_or_default()
}

pub fn parse_date(date: &str) -> Option<Date> {
    Date::parse(date.trim(), format_description!("[year]-[month]-[day]")).ok()
}

pub fn ip_preference(value: &str) -> IpPreference {
    match value {
        "v4" => IpPreference::V4,
        "v6" => IpPreference::V6,
        _ => IpPreference::Any,
    }
}

fn to_wire(row: UsenetServerRow) -> UsenetServer {
    UsenetServer {
        id: row.id.to_string(),
        name: row.name,
        host: row.host,
        port: u16::try_from(row.port).unwrap_or_default(),
        tls: row.tls,
        verify_cert: row.verify_cert,
        ip_preference: ip_preference(&row.ip_preference),
        connections: u32::try_from(row.connections).unwrap_or_default(),
        priority: u32::try_from(row.priority).unwrap_or_default(),
        backbone: row.backbone.filter(|b| !b.trim().is_empty()),
        retention_days: row.retention_days.and_then(|d| u32::try_from(d).ok()),
        expires_on: row.expires_on.map(format_date),
        quota_bytes: row.quota_bytes.and_then(|q| u64::try_from(q).ok()),
        username: row.username,
        password: row.password,
    }
}

// The engine pulls this every minute, so while SAB is enabled on the same account the engine
// shrinks itself to what SAB leaves under the cap, and grows back once SAB is disabled.
pub async fn engine_config(
    pool: &PgPool,
    http: &reqwest::Client,
) -> Result<EngineConfigResponse, RepositoryError> {
    let rows = usenet_servers::list(pool).await?;
    if !rows.iter().any(|r| r.enabled) {
        return Ok(EngineConfigResponse {
            servers: Vec::new(),
        });
    }
    let sab = sab_servers(pool, http).await;
    let allowed = caps::engine_connections(&rows, &sab);
    let servers = rows
        .into_iter()
        .filter_map(|mut row| {
            row.connections = *allowed.get(&row.id)?;
            Some(to_wire(row))
        })
        .collect();
    Ok(EngineConfigResponse { servers })
}

pub async fn warn_at_startup(pool: &PgPool) {
    let rows = match usenet_servers::list(pool).await {
        Ok(rows) => rows,
        Err(e) => {
            warn!(error = %e, "Could not check usenet connection caps");
            return;
        }
    };
    if !rows.iter().any(|r| r.enabled) {
        return;
    }
    let sab = sab_servers(pool, &crate::http::new_client()).await;
    for account in caps::accounts(&rows, &sab)
        .iter()
        .filter(|a| a.over_cap_after_yielding())
    {
        warn!(account = %account.describe(), "Usenet servers exceed the account's connection cap");
    }
}
