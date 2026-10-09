use serde::Deserialize;
use uuid::Uuid;

use super::scoring::MatchTarget;

fn default_enabled() -> bool {
    true
}

fn default_upgrade_allowed() -> bool {
    true
}

/// Request to create/update an indexer source
#[derive(Debug, Deserialize)]
pub struct IndexerSourceRequest {
    pub name: String,
    pub handler_type: String,
    pub handler_config: serde_json::Value,
    pub category_ids: Option<Vec<i32>>,
    pub media_type_ids: Option<Vec<Uuid>>,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub priority: i32,
    pub seed_ratio: Option<f64>,
    pub seed_time_minutes: Option<i32>,
    pub search_mode: Option<String>,
    pub auto_throttle: Option<bool>,
    pub api_limit_daily: Option<i32>,
    #[serde(default = "default_debrid_mode")]
    pub debrid_mode: Option<String>,
}

fn default_debrid_mode() -> Option<String> {
    Some("never".to_string())
}

/// Request to update a watch pattern (fields that Kobload can modify)
#[derive(Debug, Deserialize)]
pub struct WatchPatternUpdateRequest {
    /// Source system identifier
    pub source_system: String,
    /// Source-specific identifier
    pub source_id: String,
    /// Callback URL
    pub callback_url: String,
    /// Updated search patterns
    pub patterns: Vec<String>,
    /// Updated structured media request
    pub media_want: Option<nats_jobs::MediaWant>,
    /// Match targets
    pub match_targets: Option<Vec<MatchTarget>>,
    /// Match mode (fuzzy, exact, etc.)
    pub match_mode: String,
    /// Minimum score threshold
    pub min_score: f64,
    /// Media type ID
    pub media_type_id: Option<Uuid>,
    /// Auto download
    pub auto_download: bool,
    /// Metadata
    pub metadata: Option<serde_json::Value>,
    /// Enable/disable the pattern
    pub enabled: bool,
    /// Whether quality upgrades are allowed (default: true)
    #[serde(default = "default_upgrade_allowed")]
    pub upgrade_allowed: bool,
}
