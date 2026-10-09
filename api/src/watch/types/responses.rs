use serde::Serialize;
use time::OffsetDateTime;
use uuid::Uuid;

use super::models::{IndexerSource, WatchMatch, WatchPattern};
use nats_jobs::MatchTarget;

// Re-export shared types from nats_jobs so existing `use crate::watch::types::responses::*`
// imports continue to work throughout kobload.
pub use nats_jobs::{
    EpisodeProgress, ListWatchMatchesResponse, ListWatchPatternsResponse, WatchMatchResponse,
    WatchPatternDetailResponse, WatchPatternResponse,
};

/// Compute the status string from pattern state.
/// `has_active_download` and `has_imported_download` must be resolved by the caller.
pub fn compute_pattern_status(
    enabled: bool,
    upgrade_allowed: bool,
    upgrade_until: Option<OffsetDateTime>,
    has_active_download: bool,
    has_imported_download: bool,
    has_held_match: bool,
) -> String {
    if !enabled {
        return "paused".to_string();
    }
    if has_active_download {
        return "downloading".to_string();
    }
    if has_imported_download
        && upgrade_allowed
        && let Some(until) = upgrade_until
    {
        if until > OffsetDateTime::now_utc() {
            return "upgrading".to_string();
        }
        return "satisfied".to_string();
    }
    if has_imported_download {
        return "satisfied".to_string();
    }
    if has_held_match {
        return "held".to_string();
    }
    "searching".to_string()
}

impl From<WatchPattern> for WatchPatternResponse {
    fn from(p: WatchPattern) -> Self {
        let match_targets = p
            .match_targets
            .and_then(|v| serde_json::from_value::<Vec<MatchTarget>>(v).ok());
        let media_want = p
            .media_want
            .and_then(|v| serde_json::from_value::<nats_jobs::MediaWant>(v).ok());

        let status = compute_pattern_status(p.enabled, true, None, false, false, false);

        Self {
            id: p.id,
            source_system: p.source_system,
            source_id: p.source_id,
            callback_url: p.callback_url,
            patterns: p.patterns,
            media_want,
            match_targets,
            match_mode: p.match_mode,
            min_score: p.min_score,
            media_type_id: p.media_type_id,
            auto_download: p.auto_download,
            metadata: p.metadata,
            enabled: p.enabled,
            upgrade_allowed: true,
            upgrade_until: None,
            status,
            episode_progress: None,
            created_at: p.created_at,
            updated_at: p.updated_at,
        }
    }
}

impl From<crate::repositories::watch::WatchPatternRow> for WatchPatternResponse {
    fn from(p: crate::repositories::watch::WatchPatternRow) -> Self {
        let match_targets = p
            .match_targets
            .and_then(|v| serde_json::from_value::<Vec<MatchTarget>>(v).ok());
        let media_want = p
            .media_want
            .and_then(|v| serde_json::from_value::<nats_jobs::MediaWant>(v).ok());

        let status = compute_pattern_status(
            p.enabled,
            p.upgrade_allowed,
            p.upgrade_until,
            false,
            false,
            false,
        );

        Self {
            id: p.id,
            source_system: p.source_system,
            source_id: p.source_id,
            callback_url: p.callback_url,
            patterns: p.patterns,
            media_want,
            match_targets,
            match_mode: p.match_mode.unwrap_or_else(|| "fuzzy".to_string()),
            min_score: p.min_score,
            media_type_id: p.media_type_id,
            auto_download: p.auto_download,
            metadata: p.metadata,
            enabled: p.enabled,
            upgrade_allowed: p.upgrade_allowed,
            upgrade_until: p.upgrade_until,
            status,
            episode_progress: None,
            created_at: p.created_at,
            updated_at: p.updated_at,
        }
    }
}

/// Indexer source API response
#[derive(Debug, Serialize)]
pub struct IndexerSourceResponse {
    pub id: Uuid,
    pub name: String,
    pub handler_type: String,
    pub handler_config: serde_json::Value,
    pub category_ids: Option<Vec<i32>>,
    pub media_type_ids: Vec<Uuid>,
    pub enabled: bool,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_check_at: Option<OffsetDateTime>,
    pub last_error: Option<String>,
    pub state: Option<serde_json::Value>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    pub priority: i32,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_success_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_failure_at: Option<OffsetDateTime>,
    pub consecutive_failures: i32,
    /// Health status: "auth_failed", "healthy", "degraded", or "unhealthy"
    pub health_status: String,
    #[serde(with = "time::serde::rfc3339::option")]
    pub auth_failed_at: Option<OffsetDateTime>,
    pub seed_ratio: Option<f64>,
    pub seed_time_minutes: Option<i32>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub rate_limited_until: Option<OffsetDateTime>,
    pub capabilities: Option<serde_json::Value>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub caps_fetched_at: Option<OffsetDateTime>,
    pub api_limit_daily: Option<i32>,
    pub api_calls_today: i32,
    #[serde(with = "time::serde::rfc3339")]
    pub last_api_reset_at: OffsetDateTime,
    pub search_mode: String,
    pub auto_throttle: bool,
    pub debrid_mode: String,
}

/// A rejected credential outranks the failure count: no amount of retrying
/// clears it, so it must not read as "degraded, probably fine".
fn health_status_from(consecutive_failures: i32, auth_failed_at: Option<OffsetDateTime>) -> String {
    if auth_failed_at.is_some() {
        return "auth_failed".to_string();
    }
    match consecutive_failures {
        0 => "healthy".to_string(),
        1..=3 => "degraded".to_string(),
        _ => "unhealthy".to_string(),
    }
}

impl From<IndexerSource> for IndexerSourceResponse {
    fn from(s: IndexerSource) -> Self {
        let health_status = health_status_from(s.consecutive_failures, s.auth_failed_at);
        Self {
            id: s.id,
            name: s.name,
            handler_type: s.handler_type,
            handler_config: s.handler_config,
            category_ids: s.category_ids,
            media_type_ids: s.media_type_ids,
            enabled: s.enabled,
            last_check_at: s.last_check_at,
            last_error: s.last_error,
            state: s.state,
            created_at: s.created_at,
            priority: s.priority,
            last_success_at: s.last_success_at,
            last_failure_at: s.last_failure_at,
            consecutive_failures: s.consecutive_failures,
            health_status,
            auth_failed_at: s.auth_failed_at,
            seed_ratio: s.seed_ratio,
            seed_time_minutes: s.seed_time_minutes,
            rate_limited_until: s.rate_limited_until,
            capabilities: s.capabilities,
            caps_fetched_at: s.caps_fetched_at,
            api_limit_daily: s.api_limit_daily,
            api_calls_today: s.api_calls_today,
            last_api_reset_at: s.last_api_reset_at,
            search_mode: s.search_mode,
            auto_throttle: s.auto_throttle,
            debrid_mode: s.debrid_mode,
        }
    }
}

impl From<crate::repositories::watch::IndexerSourceRow> for IndexerSourceResponse {
    fn from(s: crate::repositories::watch::IndexerSourceRow) -> Self {
        let health_status = health_status_from(s.consecutive_failures, s.auth_failed_at);
        Self {
            id: s.id,
            name: s.name,
            handler_type: s.handler_type,
            handler_config: s.handler_config,
            category_ids: s.category_ids,
            media_type_ids: s.media_type_ids,
            enabled: s.enabled,
            last_check_at: s.last_check_at,
            last_error: s.last_error,
            state: s.state,
            created_at: s.created_at,
            priority: s.priority,
            last_success_at: s.last_success_at,
            last_failure_at: s.last_failure_at,
            consecutive_failures: s.consecutive_failures,
            health_status,
            auth_failed_at: s.auth_failed_at,
            seed_ratio: s.seed_ratio,
            seed_time_minutes: s.seed_time_minutes,
            rate_limited_until: s.rate_limited_until,
            capabilities: s.capabilities,
            caps_fetched_at: s.caps_fetched_at,
            api_limit_daily: s.api_limit_daily,
            api_calls_today: s.api_calls_today,
            last_api_reset_at: s.last_api_reset_at,
            search_mode: s.search_mode,
            auto_throttle: s.auto_throttle,
            debrid_mode: s.debrid_mode,
        }
    }
}

impl From<WatchMatch> for WatchMatchResponse {
    fn from(m: WatchMatch) -> Self {
        Self {
            id: m.id,
            watch_pattern_id: m.watch_pattern_id,
            indexer_source_id: m.indexer_source_id,
            release_guid: m.release_guid,
            release_title: m.release_title,
            release_data: m.release_data,
            match_score: m.match_score,
            matched_pattern: m.matched_pattern,
            action: m.action,
            download_id: m.download_id,
            callback_sent_at: m.callback_sent_at,
            callback_error: m.callback_error,
            quality_score: None,
            rejected: false,
            rejected_at: None,
            rejected_reason: None,
            held: false,
            hold_until: None,
            created_at: m.created_at,
            import_confirmed_at: None,
            episode_key: None,
        }
    }
}

impl From<crate::repositories::watch::WatchMatchRow> for WatchMatchResponse {
    fn from(m: crate::repositories::watch::WatchMatchRow) -> Self {
        Self {
            id: m.id,
            watch_pattern_id: Some(m.watch_pattern_id),
            indexer_source_id: m.indexer_source_id,
            release_guid: m.release_guid,
            release_title: m.release_title,
            release_data: m.release_data.unwrap_or_default(),
            match_score: m.match_score,
            matched_pattern: Some(m.matched_pattern),
            action: m.action,
            download_id: m.download_id,
            callback_sent_at: m.callback_sent_at,
            callback_error: m.callback_error,
            quality_score: m.quality_score,
            rejected: m.rejected,
            rejected_at: m.rejected_at,
            rejected_reason: m.rejected_reason,
            held: m.held,
            hold_until: m.hold_until,
            created_at: m.created_at,
            import_confirmed_at: m.import_confirmed_at,
            episode_key: m.episode_key,
        }
    }
}
