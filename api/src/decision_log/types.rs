//! Types for decision log entries.

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

/// Itemized quality score breakdown — every component that contributed to the final score.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct QualityScoreBreakdown {
    #[serde(default, skip_serializing_if = "is_zero")]
    pub resolution_tier: f64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub min_resolution_check: f64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub max_resolution_check: f64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub preferred_source: f64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub rejected_source: f64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub preferred_codec: f64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub preferred_audio: f64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub hardcoded_subs: f64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub remux_rejection: f64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub proper_repack: f64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub real: f64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub remux_bonus: f64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub dolby_vision: f64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub hdr10plus: f64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub hdr10: f64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub atmos_dtsx: f64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub internal: f64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub nuked: f64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub language: f64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub rejected_language: f64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub rejected_term: f64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub trash_filter: f64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub trash_tier_bonus: f64,
    pub total: f64,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub rejected: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rejected_reason: Option<String>,
}

fn is_zero(v: &f64) -> bool {
    *v == 0.0
}

/// Result of structural matching against MediaWant.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct StructuralResult {
    /// Whether structural validation was performed (false if no MediaWant).
    pub checked: bool,
    /// Whether it passed.
    pub matched: bool,
    /// Rejection reason if matched=false (e.g., "season_mismatch").
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reject_reason: Option<String>,
    // Parsed fields from release title
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parsed_season: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parsed_episode: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parsed_year: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parsed_title: Option<String>,
    // What was wanted
    #[serde(skip_serializing_if = "Option::is_none")]
    pub want_season: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub want_episode: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub want_year: Option<i32>,
}

/// A provider call record (in-memory, before DB flush).
#[derive(Debug, Clone)]
pub struct LogProviderCall {
    pub id: Uuid,
    pub indexer_source_id: Option<Uuid>,
    pub source_name: String,
    pub handler_type: String,
    pub search_type: Option<String>,
    pub query: Option<String>,
    pub categories: Option<serde_json::Value>,
    pub external_ids: Option<serde_json::Value>,
    pub results_count: i32,
    pub results_after_dedup: i32,
    pub status: String,
    pub error_message: Option<String>,
    pub duration_ms: Option<i32>,
}

/// A release evaluation record (in-memory, before DB flush).
#[derive(Debug, Clone)]
pub struct LogRelease {
    pub provider_call_id: Option<Uuid>,
    pub release_title: String,
    pub release_guid: Option<String>,
    pub release_size: Option<i64>,
    pub protocol: Option<String>,
    pub indexer_name: Option<String>,
    pub match_score: Option<f64>,
    pub matched_pattern: Option<String>,
    pub match_mode: Option<String>,
    pub quality_score: Option<f64>,
    pub quality_score_breakdown: Option<QualityScoreBreakdown>,
    pub total_score: Option<f64>,
    pub parsed_quality: Option<serde_json::Value>,
    pub structural_result: Option<StructuralResult>,
    pub decision: String,
    pub decision_reason: Option<String>,
    pub episode_key: Option<String>,
    pub is_best: bool,
    pub release_data: Option<serde_json::Value>,
}

/// Owned payload ready for background DB flush.
pub struct FlushPayload {
    pub session_id: Uuid,
    pub trigger_type: String,
    pub watch_pattern_id: Option<Uuid>,
    pub indexer_source_id: Option<Uuid>,
    pub is_test: bool,
    pub outcome: Option<String>,
    pub download_id: Option<Uuid>,
    pub releases_evaluated: i32,
    pub search_queries: Option<serde_json::Value>,
    pub error_message: Option<String>,
    pub started_at: OffsetDateTime,
    pub finished_at: Option<OffsetDateTime>,
    pub duration_ms: Option<i32>,
    pub provider_calls: Vec<LogProviderCall>,
    pub releases: Vec<LogRelease>,
}

// -- API response types --

/// Session list item (for paginated listing).
#[derive(Debug, Serialize)]
pub struct SessionListItem {
    pub id: Uuid,
    pub trigger_type: String,
    pub watch_pattern_id: Option<Uuid>,
    pub indexer_source_id: Option<Uuid>,
    pub is_test: bool,
    pub outcome: Option<String>,
    pub download_id: Option<Uuid>,
    pub releases_evaluated: i32,
    pub search_queries: Option<serde_json::Value>,
    pub error_message: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub started_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub finished_at: Option<OffsetDateTime>,
    pub duration_ms: Option<i32>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    // Joined display names
    pub pattern_name: Option<String>,
    pub source_name: Option<String>,
    /// Metadata from the watch pattern (title, author, etc.)
    pub pattern_metadata: Option<serde_json::Value>,
}

/// Full session detail with children.
#[derive(Debug, Serialize)]
pub struct SessionDetail {
    pub id: Uuid,
    pub trigger_type: String,
    pub watch_pattern_id: Option<Uuid>,
    pub indexer_source_id: Option<Uuid>,
    pub is_test: bool,
    pub outcome: Option<String>,
    pub download_id: Option<Uuid>,
    pub releases_evaluated: i32,
    pub search_queries: Option<serde_json::Value>,
    pub error_message: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub started_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub finished_at: Option<OffsetDateTime>,
    pub duration_ms: Option<i32>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    pub pattern_name: Option<String>,
    pub source_name: Option<String>,
    /// Metadata from the watch pattern (title, author, etc.)
    pub pattern_metadata: Option<serde_json::Value>,
    pub provider_calls: Vec<ProviderCallDetail>,
    pub releases: Vec<ReleaseDetail>,
}

/// Provider call in session detail response.
#[derive(Debug, Serialize)]
pub struct ProviderCallDetail {
    pub id: Uuid,
    pub indexer_source_id: Option<Uuid>,
    pub source_name: String,
    pub handler_type: String,
    pub search_type: Option<String>,
    pub query: Option<String>,
    pub categories: Option<serde_json::Value>,
    pub external_ids: Option<serde_json::Value>,
    pub results_count: i32,
    pub results_after_dedup: i32,
    pub status: String,
    pub error_message: Option<String>,
    pub duration_ms: Option<i32>,
}

/// Release evaluation in session detail response.
#[derive(Debug, Serialize)]
pub struct ReleaseDetail {
    pub id: Uuid,
    pub provider_call_id: Option<Uuid>,
    pub release_title: String,
    pub release_guid: Option<String>,
    pub release_size: Option<i64>,
    pub protocol: Option<String>,
    pub indexer_name: Option<String>,
    pub match_score: Option<f64>,
    pub matched_pattern: Option<String>,
    pub match_mode: Option<String>,
    pub quality_score: Option<f64>,
    pub quality_score_breakdown: Option<serde_json::Value>,
    pub total_score: Option<f64>,
    pub parsed_quality: Option<serde_json::Value>,
    pub structural_result: Option<serde_json::Value>,
    pub decision: String,
    pub decision_reason: Option<String>,
    pub episode_key: Option<String>,
    pub is_best: bool,
    pub release_data: Option<serde_json::Value>,
}

/// Export format — full session tree as a single JSON blob.
#[derive(Debug, Serialize)]
pub struct SessionExport {
    pub session: SessionDetail,
}

/// Logging settings.
#[derive(Debug, Serialize, Deserialize)]
pub struct DecisionLogSettings {
    pub search_logging_enabled: bool,
    pub feed_logging_enabled: bool,
    pub debrid_logging_enabled: bool,
}
