use serde::{Deserialize, Serialize, Serializer};
use uuid::Uuid;

// =============================================================================
// Download Client Progress (structured type for client status)
// =============================================================================

/// Structured progress information from download clients.
/// All fields are optional as different clients support different capabilities.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DownloadClientProgress {
    /// Estimated completion time as UTC timestamp (ISO 8601).
    /// Client can calculate countdown locally without constant updates.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub eta_time: Option<String>,

    /// Current download rate in bytes per second.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub download_rate: Option<i64>,

    /// Total download size in bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub total_size: Option<i64>,
}

impl DownloadClientProgress {
    /// Parse from raw client_data JSON (extracts normalized fields).
    pub fn from_json(value: &serde_json::Value) -> Self {
        Self {
            eta_time: value
                .get("eta_time")
                .and_then(|v| v.as_str())
                .map(String::from),
            download_rate: value.get("download_rate").and_then(|v| v.as_i64()),
            total_size: value.get("total_size").and_then(|v| v.as_i64()),
        }
    }
}

/// Custom serializer for client_data that converts JSON blob to structured type.
fn serialize_client_progress<S>(
    value: &Option<serde_json::Value>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match value {
        Some(json) => {
            let progress = DownloadClientProgress::from_json(json);
            progress.serialize(serializer)
        }
        None => serializer.serialize_none(),
    }
}

/// Reference to an API key with id and name
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct ApiKeyRef {
    pub id: Uuid,
    pub name: String,
}

/// Generic category type that works across different indexers
#[derive(Debug, Serialize, Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct Category {
    pub id: i32,
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sub_categories: Vec<Category>,
}

/// Torrent-specific statistics (only present for torrent releases)
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct TorrentStats {
    pub seeders: Option<i32>,
    pub peers: Option<i32>,
    pub leechers: Option<i32>,
}

/// Usenet-specific statistics (only present for usenet releases)
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct UsenetStats {
    pub grabs: Option<i32>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct SearchResult {
    // Identity
    pub id: String,
    pub guid: Option<String>,

    // Content
    pub title: String,
    pub file_name: Option<String>,
    pub format: Option<String>,
    pub size: i64,
    pub files: Option<i32>,

    // Source
    pub indexer: String,
    pub indexer_type: String,
    pub indexer_id: Option<String>,
    pub info_url: Option<String>,

    // Download
    pub protocol: String,
    pub download_url: Option<String>,
    pub magnet_url: Option<String>,
    pub info_hash: Option<String>,

    // Stats
    pub publish_date: Option<String>,
    /// Torrent-specific stats (only present for protocol="torrent")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub torrent: Option<TorrentStats>,
    /// Usenet-specific stats (only present for protocol="usenet")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usenet: Option<UsenetStats>,

    // Kobload context
    pub media_type: Option<String>,

    // Categories (generic, preserves hierarchy)
    pub categories: Option<Vec<Category>>,
}

#[derive(Debug, Deserialize)]
pub struct ProwlarrSearchRequest {
    pub q: String,
    pub media_type: Option<String>,
    pub protocol: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct DownloadResponse {
    pub id: Uuid,
    pub status: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DownloadRequest {
    pub source: String,
    pub protocol: String,
    /// Display name for this download
    pub name: String,
    pub download_url: String,
    pub download_client_id: Option<Uuid>,
    pub callback_url: String,
    pub auth_token: Option<String>,
    /// Arbitrary metadata blob - passed back to callback as-is
    pub metadata: serde_json::Value,
    pub release: Option<serde_json::Value>,
    /// Quality profile to use for file scoring at completion
    pub quality_profile_id: Option<Uuid>,
    /// Media kind (book, tv, movie) for format-aware file scoring
    pub media_kind: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct DownloadDetail {
    pub id: Uuid,
    pub source: String,
    pub protocol: String,
    pub profile_id: Option<Uuid>,
    pub name: String,
    pub download_url: String,
    pub callback_url: String,
    pub download_client_id: Option<Uuid>,
    pub external_id: Option<String>,
    pub metadata: serde_json::Value,
    pub status: String,
    pub progress: Option<f64>,
    pub file_path: Option<String>,
    pub error: Option<String>,
    pub release_payload: Option<serde_json::Value>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: time::OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: time::OffsetDateTime,
}

#[derive(Debug, Clone, Serialize)]
pub struct DownloadListItem {
    pub id: Uuid,
    pub name: String,
    pub protocol: String,
    pub profile_id: Option<Uuid>,
    pub status: String,
    pub progress: Option<f64>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub last_poll_at: Option<time::OffsetDateTime>,
    pub last_poll_error: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: time::OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: time::OffsetDateTime,
    // Extended fields for detailed view
    pub download_client_id: Option<Uuid>,
    pub external_id: Option<String>,
    pub file_path: Option<String>,
    pub error: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub callback_sent_at: Option<time::OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub import_confirmed_at: Option<time::OffsetDateTime>,
    pub files_json: Option<serde_json::Value>,
    pub release_payload: Option<serde_json::Value>,
    /// Parsed release title (show name, season, episodes, codec, etc.)
    pub release_interpretation_json: Option<serde_json::Value>,
    /// Client-provided metadata blob (echoed back in callbacks)
    pub metadata: serde_json::Value,
    // Callback retry tracking
    pub callback_attempts: i32,
    #[serde(with = "time::serde::rfc3339::option")]
    pub callback_last_attempt_at: Option<time::OffsetDateTime>,
    pub callback_error: Option<String>,
    /// Files scan error (recoverable - e.g., network share not mounted)
    pub files_scan_error: Option<String>,
    /// Structured progress information from download client.
    /// Contains normalized fields like eta_time, download_rate, total_size.
    #[serde(serialize_with = "serialize_client_progress")]
    pub client_progress: Option<serde_json::Value>,
    // Joined fields
    pub download_client_name: Option<String>,
    pub api_key_id: Option<Uuid>,
    pub api_key_name: Option<String>,
    // Watch pattern link (from watch_matches join)
    pub watch_pattern_source_system: Option<String>,
    pub watch_pattern_source_id: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct DownloadEvent {
    pub id: Uuid,
    pub event_type: String,
    pub message: Option<String>,
    pub details: Option<serde_json::Value>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: time::OffsetDateTime,
}

#[derive(Debug, Serialize)]
pub struct ProwlarrConfig {
    pub base_url: Option<String>,
    pub api_key: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DownloadClientConfig {
    pub id: Uuid,
    pub name: String,
    pub client_type: String,
    pub protocol: String,
    pub base_url: String,
    pub api_key: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub category: Option<String>,
    pub download_dir: Option<String>,
    pub is_default: bool,
    pub delete_after_import: bool,
    pub seed_ratio: Option<f64>,
    pub seed_time_minutes: Option<i32>,
    pub enabled: bool,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: time::OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: time::OffsetDateTime,
}

#[derive(Debug, Serialize)]
pub struct TestConnectionResult {
    pub success: bool,
    pub message: String,
}

/// A field that can be configured for a download client
#[derive(Debug, Clone, Serialize)]
pub struct DownloadClientField {
    /// Field name (matches the JSON/form field name)
    pub name: String,
    /// Display label
    pub label: String,
    /// Field type: "text", "password", "number", "checkbox"
    pub field_type: String,
    /// Placeholder text (optional)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    /// Whether the field is required
    #[serde(default)]
    pub required: bool,
}

/// A supported download client type
#[derive(Debug, Clone, Serialize)]
pub struct DownloadClientType {
    /// Unique identifier for this client type (e.g. "transmission", "sabnzbd")
    pub id: String,
    /// Display name (e.g. "Transmission", "SABnzbd")
    pub name: String,
    /// Protocol this client supports (e.g. "torrent", "usenet")
    pub protocol: String,
    /// Fields specific to this client type (beyond the common fields)
    pub fields: Vec<DownloadClientField>,
}

impl DownloadClientType {
    /// Returns all supported download client types
    pub fn all() -> Vec<Self> {
        vec![
            Self {
                id: "transmission".to_string(),
                name: "Transmission".to_string(),
                protocol: "torrent".to_string(),
                fields: vec![
                    DownloadClientField {
                        name: "base_url".to_string(),
                        label: "Base URL".to_string(),
                        field_type: "text".to_string(),
                        placeholder: Some("http://localhost:9091".to_string()),
                        required: true,
                    },
                    DownloadClientField {
                        name: "username".to_string(),
                        label: "Username".to_string(),
                        field_type: "text".to_string(),
                        placeholder: Some("Optional".to_string()),
                        required: false,
                    },
                    DownloadClientField {
                        name: "password".to_string(),
                        label: "Password".to_string(),
                        field_type: "password".to_string(),
                        placeholder: Some("Optional".to_string()),
                        required: false,
                    },
                    DownloadClientField {
                        name: "category".to_string(),
                        label: "Category".to_string(),
                        field_type: "text".to_string(),
                        placeholder: Some("Optional".to_string()),
                        required: false,
                    },
                    DownloadClientField {
                        name: "download_dir".to_string(),
                        label: "Download dir".to_string(),
                        field_type: "text".to_string(),
                        placeholder: Some("Optional".to_string()),
                        required: false,
                    },
                    DownloadClientField {
                        name: "seed_ratio".to_string(),
                        label: "Seed ratio limit".to_string(),
                        field_type: "text".to_string(),
                        placeholder: Some("Optional".to_string()),
                        required: false,
                    },
                    DownloadClientField {
                        name: "seed_time_minutes".to_string(),
                        label: "Seed time limit (min)".to_string(),
                        field_type: "text".to_string(),
                        placeholder: Some("Optional".to_string()),
                        required: false,
                    },
                ],
            },
            Self {
                id: "sabnzbd".to_string(),
                name: "SABnzbd".to_string(),
                protocol: "usenet".to_string(),
                fields: vec![
                    DownloadClientField {
                        name: "base_url".to_string(),
                        label: "Base URL".to_string(),
                        field_type: "text".to_string(),
                        placeholder: Some("http://localhost:8080".to_string()),
                        required: true,
                    },
                    DownloadClientField {
                        name: "api_key".to_string(),
                        label: "API key".to_string(),
                        field_type: "text".to_string(),
                        placeholder: Some("SABnzbd API key".to_string()),
                        required: true,
                    },
                    DownloadClientField {
                        name: "category".to_string(),
                        label: "Category".to_string(),
                        field_type: "text".to_string(),
                        placeholder: Some("Optional".to_string()),
                        required: false,
                    },
                ],
            },
            Self {
                id: "debrid".to_string(),
                name: "Debrid".to_string(),
                protocol: "torrent".to_string(),
                fields: vec![
                    DownloadClientField {
                        name: "api_key".to_string(),
                        label: "Debrid Provider ID".to_string(),
                        field_type: "text".to_string(),
                        placeholder: Some("UUID of configured debrid provider".to_string()),
                        required: true,
                    },
                    DownloadClientField {
                        name: "download_dir".to_string(),
                        label: "Download directory".to_string(),
                        field_type: "text".to_string(),
                        placeholder: Some("/data/debrid-downloads".to_string()),
                        required: true,
                    },
                ],
            },
        ]
    }

    /// Get a client type by ID
    pub fn get(id: &str) -> Option<Self> {
        Self::all().into_iter().find(|t| t.id == id)
    }
}

#[derive(Debug, Deserialize)]
pub struct CompleteDownloadRequest {
    pub status: String,
    pub file_path: String,
}

/// Events sent over the download stream SSE endpoint
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DownloadStreamEvent {
    /// Full snapshot of all downloads (sent on connect)
    Snapshot { downloads: Vec<DownloadListItem> },
    /// A download was created
    Created { download: DownloadListItem },
    /// A download was updated (status, progress, etc.)
    Updated { download: DownloadListItem },
    /// A stream option finished debrid cache warming and is ready for instant playback.
    StreamWarmReady {
        info_hash: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        file_id: Option<i32>,
        debrid_provider_id: uuid::Uuid,
        debrid_provider: String,
    },
}

/// Events sent over the watch matches stream SSE endpoint
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WatchStreamEvent {
    /// Full snapshot of recent matches (sent on connect)
    Snapshot {
        matches: Vec<crate::watch::WatchMatchResponse>,
    },
    /// A new match was created
    Created {
        watch_match: Box<crate::watch::WatchMatchResponse>,
    },
}

/// Events sent over the watch patterns stream SSE endpoint
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WatchPatternStreamEvent {
    /// Full snapshot of patterns (sent on connect)
    Snapshot {
        patterns: Vec<crate::watch::WatchPatternResponse>,
    },
    /// A pattern was created
    Created {
        watch_pattern: Box<crate::watch::WatchPatternResponse>,
    },
    /// A pattern was updated
    Updated {
        watch_pattern: Box<crate::watch::WatchPatternResponse>,
    },
    /// A pattern was deleted
    Deleted { id: Uuid },
}
