//! Shared protocol preference resolution.
//!
//! Used by watch holds, immediate search, and anywhere else that needs
//! to know the preferred download protocol and delay configuration.

use uuid::Uuid;

use crate::repositories::app_settings;
use crate::repositories::media_types as media_types_repo;

/// Resolved protocol preference configuration.
pub struct ProtocolConfig {
    /// Ordered list of protocols by priority (first = highest).
    pub priority: Vec<String>,
    /// Minutes to hold a non-preferred protocol match before grabbing.
    pub delay_minutes: i32,
}

impl ProtocolConfig {
    /// The top-priority protocol, if any.
    pub fn preferred(&self) -> Option<&str> {
        self.priority.first().map(|s| s.as_str())
    }
}

/// Resolve protocol priority and delay from per-media-type config or global defaults.
pub async fn resolve_protocol_config(
    pool: &sqlx::PgPool,
    media_type_id: Option<Uuid>,
) -> Option<ProtocolConfig> {
    // Per-media-type config first
    if let Some(mt_id) = media_type_id
        && let Ok(Some(config)) = media_types_repo::get_by_id(pool, mt_id).await
        && !config.protocol_priority.is_empty()
    {
        return Some(ProtocolConfig {
            priority: config.protocol_priority,
            delay_minutes: config.protocol_delay_minutes.unwrap_or(0),
        });
    }
    // Global defaults
    if let Ok(defaults) = app_settings::get_protocol_defaults(pool).await
        && !defaults.protocol_priority.is_empty()
    {
        return Some(ProtocolConfig {
            priority: defaults.protocol_priority,
            delay_minutes: defaults.protocol_delay_minutes.unwrap_or(0),
        });
    }
    // Fallback: prefer usenet (most common preference, avoids ratio concerns)
    Some(ProtocolConfig {
        priority: vec!["usenet".to_string()],
        delay_minutes: 0,
    })
}
