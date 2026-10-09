//! Repository for the app_settings key-value store.

use super::Result;
use sqlx::{Executor, Postgres};

/// Get a setting value by key.
pub async fn get<'e, E>(exec: E, key: &str) -> Result<Option<serde_json::Value>>
where
    E: Executor<'e, Database = Postgres>,
{
    let row = sqlx::query_scalar!("SELECT value FROM app_settings WHERE key = $1", key)
        .fetch_optional(exec)
        .await?;

    Ok(row)
}

/// Set a setting value (upsert).
pub async fn set<'e, E>(exec: E, key: &str, value: &serde_json::Value) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query!(
        r#"
        INSERT INTO app_settings (key, value, updated_at)
        VALUES ($1, $2, NOW())
        ON CONFLICT (key) DO UPDATE SET value = $2, updated_at = NOW()
        "#,
        key,
        value,
    )
    .execute(exec)
    .await?;

    Ok(())
}

/// Well-known settings keys.
pub mod keys {
    pub const PROTOCOL_DEFAULTS: &str = "protocol_defaults";
    pub const UPGRADE_DEFAULTS: &str = "upgrade_defaults";
    pub const SEARCH_LOGGING_ENABLED: &str = "search_logging_enabled";
    pub const FEED_LOGGING_ENABLED: &str = "feed_logging_enabled";
    pub const DEBRID_LOGGING_ENABLED: &str = "debrid_logging_enabled";
    pub const KOBLEM_SETTINGS: &str = "koblem_settings";
}

/// Protocol defaults stored in app_settings.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize, Default)]
pub struct ProtocolDefaults {
    #[serde(default)]
    pub protocol_priority: Vec<String>,
    pub protocol_delay_minutes: Option<i32>,
}

/// Get global protocol defaults.
pub async fn get_protocol_defaults<'e, E>(exec: E) -> Result<ProtocolDefaults>
where
    E: Executor<'e, Database = Postgres>,
{
    match get(exec, keys::PROTOCOL_DEFAULTS).await? {
        Some(val) => Ok(serde_json::from_value(val).unwrap_or_default()),
        None => Ok(ProtocolDefaults::default()),
    }
}

/// Set global protocol defaults.
pub async fn set_protocol_defaults<'e, E>(exec: E, defaults: &ProtocolDefaults) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    let val = serde_json::to_value(defaults).unwrap_or_default();
    set(exec, keys::PROTOCOL_DEFAULTS, &val).await
}

/// Upgrade defaults stored in app_settings.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct UpgradeDefaults {
    /// How many hours to keep watching for upgrades after first import (default: 720 = 30 days).
    pub upgrade_window_hours: Option<i32>,
    /// Minimum quality score improvement required to trigger an upgrade (default: 0.10).
    /// Prevents trivial upgrades from near-identical releases (e.g., different audio codecs).
    #[serde(default)]
    pub min_upgrade_margin: Option<f64>,
    /// Grace window (minutes) to wait before accepting a FIRST grab that is below
    /// the profile's target resolution tier, giving a target-tier release time to
    /// appear (default: 30). Measured from the release's publish_date. After the
    /// window, the below-target release is accepted so we're not left with nothing.
    /// 0 disables the wait.
    #[serde(default)]
    pub below_target_grace_minutes: Option<i64>,
}

impl Default for UpgradeDefaults {
    fn default() -> Self {
        Self {
            upgrade_window_hours: Some(720),
            min_upgrade_margin: Some(0.10),
            below_target_grace_minutes: Some(30),
        }
    }
}

/// Get global upgrade defaults.
pub async fn get_upgrade_defaults<'e, E>(exec: E) -> Result<UpgradeDefaults>
where
    E: Executor<'e, Database = Postgres>,
{
    match get(exec, keys::UPGRADE_DEFAULTS).await? {
        Some(val) => Ok(serde_json::from_value(val).unwrap_or_default()),
        None => Ok(UpgradeDefaults::default()),
    }
}

/// Set global upgrade defaults.
pub async fn set_upgrade_defaults<'e, E>(exec: E, defaults: &UpgradeDefaults) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    let val = serde_json::to_value(defaults).unwrap_or_default();
    set(exec, keys::UPGRADE_DEFAULTS, &val).await
}

/// Get whether search logging is enabled.
pub async fn get_search_logging_enabled<'e, E>(exec: E) -> Result<bool>
where
    E: Executor<'e, Database = Postgres>,
{
    match get(exec, keys::SEARCH_LOGGING_ENABLED).await? {
        Some(val) => Ok(val.as_bool().unwrap_or(false)),
        None => Ok(false),
    }
}

/// Get whether feed logging is enabled.
pub async fn get_feed_logging_enabled<'e, E>(exec: E) -> Result<bool>
where
    E: Executor<'e, Database = Postgres>,
{
    match get(exec, keys::FEED_LOGGING_ENABLED).await? {
        Some(val) => Ok(val.as_bool().unwrap_or(false)),
        None => Ok(false),
    }
}

/// Get whether debrid/stream logging is enabled.
pub async fn get_debrid_logging_enabled<'e, E>(exec: E) -> Result<bool>
where
    E: Executor<'e, Database = Postgres>,
{
    match get(exec, keys::DEBRID_LOGGING_ENABLED).await? {
        Some(val) => Ok(val.as_bool().unwrap_or(false)),
        None => Ok(false),
    }
}

/// Koblem (local LLM review) settings stored in app_settings.
#[derive(Debug, Clone, Default, serde::Deserialize, serde::Serialize)]
pub struct KoblemSettings {
    /// URL of the koblem service (e.g. "http://koblem:8090").
    #[serde(default)]
    pub service_url: Option<String>,
    /// Whether LLM-based candidate review is enabled.
    #[serde(default)]
    pub enabled: bool,
}

/// Get koblem settings.
pub async fn get_koblem_settings<'e, E>(exec: E) -> Result<KoblemSettings>
where
    E: Executor<'e, Database = Postgres>,
{
    match get(exec, keys::KOBLEM_SETTINGS).await? {
        Some(val) => Ok(serde_json::from_value(val).unwrap_or_default()),
        None => Ok(KoblemSettings::default()),
    }
}

/// Set koblem settings.
pub async fn set_koblem_settings<'e, E>(exec: E, settings: &KoblemSettings) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    let val = serde_json::to_value(settings).unwrap_or_default();
    set(exec, keys::KOBLEM_SETTINGS, &val).await
}
