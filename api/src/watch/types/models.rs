use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

/// Source of an external identifier (IMDB, TMDB, etc.).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExternalIdSource {
    Imdb,
    Tmdb,
    Tvdb,
    TvMaze,
    Isbn,
    #[serde(untagged)]
    Other(String),
}

/// An external identifier for a release (e.g., IMDB ID, TMDB ID).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExternalId {
    pub source: ExternalIdSource,
    pub id: String,
}

/// Structured media type information extracted from a release.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ReleaseMedia {
    Tv {
        season: Option<i32>,
        episode: Option<i32>,
        year: Option<i32>,
    },
    Movie {
        year: Option<i32>,
    },
    Book {
        author: Option<String>,
    },
    Unknown,
}

/// Database model for watch patterns
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct WatchPattern {
    pub id: Uuid,
    pub source_system: String,
    pub source_id: String,
    pub callback_url: String,
    pub patterns: Vec<String>,
    /// Structured media request (stored as JSON)
    pub media_want: Option<serde_json::Value>,
    /// Match targets for scoring (stored as JSONB array of MatchTarget)
    pub match_targets: Option<serde_json::Value>,
    pub match_mode: String,
    pub min_score: Option<f64>,
    pub media_type_id: Option<Uuid>,
    pub auto_download: bool,
    pub metadata: Option<serde_json::Value>,
    pub enabled: bool,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

/// Database model for indexer sources
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct IndexerSource {
    pub id: Uuid,
    pub name: String,
    pub handler_type: String,
    pub handler_config: serde_json::Value,
    pub category_ids: Option<Vec<i32>>,
    pub media_type_ids: Vec<Uuid>,
    pub enabled: bool,
    pub last_check_at: Option<OffsetDateTime>,
    pub last_error: Option<String>,
    pub state: Option<serde_json::Value>,
    pub created_at: OffsetDateTime,
    pub priority: i32,
    pub last_success_at: Option<OffsetDateTime>,
    pub last_failure_at: Option<OffsetDateTime>,
    pub consecutive_failures: i32,
    pub seed_ratio: Option<f64>,
    pub seed_time_minutes: Option<i32>,
    pub rate_limited_until: Option<OffsetDateTime>,
    pub auth_failed_at: Option<OffsetDateTime>,
    pub capabilities: Option<serde_json::Value>,
    pub caps_fetched_at: Option<OffsetDateTime>,
    pub api_limit_daily: Option<i32>,
    pub api_calls_today: i32,
    pub last_api_reset_at: OffsetDateTime,
    pub search_mode: String,
    pub auto_throttle: bool,
    pub debrid_mode: String,
}

/// Database model for watch matches
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct WatchMatch {
    pub id: Uuid,
    pub watch_pattern_id: Option<Uuid>,
    pub indexer_source_id: Option<Uuid>,
    pub release_guid: String,
    pub release_title: String,
    pub release_data: serde_json::Value,
    pub match_score: f64,
    pub matched_pattern: Option<String>,
    pub action: String,
    pub download_id: Option<Uuid>,
    pub callback_sent_at: Option<OffsetDateTime>,
    pub callback_error: Option<String>,
    pub created_at: OffsetDateTime,
}

/// Watermark state stored in indexer_sources.state for RSS gap detection.
///
/// Tracks the newest seen release to detect if items were missed between polls.
/// Used by all RSS handlers (prowlarr_rss, newznab, torznab, cardigann).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct RssWatermark {
    /// GUID of the newest release we've seen
    #[serde(default)]
    pub last_seen_guid: Option<String>,
    /// Publish date of the newest release (ISO 8601 string)
    #[serde(default)]
    pub last_seen_publish_date: Option<String>,
    /// Per-indexer retry-after deadlines (indexer_id -> RFC 3339 datetime).
    /// When an indexer returns 429 with Retry-After, we record when to resume.
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub indexer_retry_after: std::collections::HashMap<String, String>,
}

/// Result from RSS check including watermark update info.
pub struct RssCheckResult {
    pub releases: Vec<Release>,
    pub new_watermark: Option<RssWatermark>,
    pub gap_detected: bool,
}

/// Normalized release from any indexer source.
/// This is the generic type that all indexer implementations convert to.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Release {
    pub guid: String,
    pub title: String,
    pub file_name: Option<String>,
    pub download_url: Option<String>,
    pub magnet_url: Option<String>,
    pub info_url: Option<String>,
    pub size: i64,
    pub publish_date: String,
    pub category_ids: Vec<i32>,
    pub indexer: String,
    pub protocol: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub info_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seeders: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub peers: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leechers: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grabs: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub votes: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indexer_id: Option<i32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub indexer_source_id: Option<Uuid>,
    pub raw_data: serde_json::Value,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub external_ids: Vec<ExternalId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub media: Option<ReleaseMedia>,
}

/// A release with kobload's scoring decisions attached.
///
/// Populated by the scoring pipeline so downstream consumers (koblem review,
/// decision log) don't need to re-process releases.
///
/// This is the canonical output type for the unified scoring pipeline.
/// All scoring paths (want, backlog, RSS, hold expiry, stream debrid)
/// produce this type.
#[derive(Debug, Clone, Serialize)]
pub struct ProcessedRelease {
    pub release: std::sync::Arc<Release>,

    // ── Decision ──
    /// Typed decision enum.
    pub decision: nats_jobs::ScoringDecision,
    /// All rejection reasons (empty for candidates).
    pub reject_reasons: Vec<nats_jobs::RejectReason>,

    // ── Match scoring ──
    pub match_score: Option<f64>,
    pub matched_pattern: Option<String>,
    pub matched_target: Option<nats_jobs::MatchedTarget>,

    // ── Quality scoring ──
    pub parsed_quality: Option<nats_jobs::ParsedQuality>,
    pub quality_score: Option<f64>,
    pub quality_rejected: bool,
    pub quality_reject_reasons: Vec<nats_jobs::QualityRejectReason>,
    /// Itemised quality score, for the decision log. Not part of the scoring
    /// output, so the golden replay doesn't compare it.
    #[serde(skip)]
    pub quality_breakdown: Option<crate::decision_log::QualityScoreBreakdown>,
    #[serde(skip)]
    pub match_explanation: Option<crate::services::want::scoring::MatchExplanation>,

    // ── Structural scoring ──
    pub structural_rejected: bool,
    pub structural_reject_reason: Option<nats_jobs::StructuralRejectReason>,

    // ── Selection data (replaces BacklogCandidate, ScoredCandidate) ──
    pub total_score: f64,
    pub episode_key: Option<String>,
    pub is_preferred_protocol: bool,
    pub structural_bonus: f64,
    pub seeder_bonus: f64,
    pub freshness_bonus: f64,

    // ── Contextual analysis (populated by score_batch phase 2) ──
    pub context_flags: Vec<nats_jobs::ContextFlag>,
}

impl ProcessedRelease {
    /// Whether this release is a candidate (passed all scoring gates).
    pub fn is_candidate(&self) -> bool {
        self.decision == nats_jobs::ScoringDecision::Candidate
    }

    /// Computed total score (match + quality + bonuses).
    pub fn computed_total_score(&self) -> f64 {
        self.match_score.unwrap_or(0.0)
            + self.quality_score.unwrap_or(0.0)
            + self.structural_bonus
            + self.seeder_bonus
            + self.freshness_bonus
    }

    pub fn is_better_than(&self, other: &Self) -> bool {
        self.rank_cmp(other) == std::cmp::Ordering::Greater
    }

    pub fn rank_cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.is_preferred_protocol
            .cmp(&other.is_preferred_protocol)
            .then(self.total_score.total_cmp(&other.total_score))
    }

    /// Convert to the wire DTO for API responses and NATS messages.
    pub fn to_dto(&self) -> nats_jobs::ProcessedReleaseDto {
        nats_jobs::ProcessedReleaseDto {
            guid: self.release.guid.clone(),
            title: self.release.title.clone(),
            file_name: self.release.file_name.clone(),
            size: self.release.size,
            indexer: self.release.indexer.clone(),
            protocol: self.release.protocol.clone(),
            download_url: self.release.download_url.clone(),
            magnet_url: self.release.magnet_url.clone(),
            info_url: self.release.info_url.clone(),
            publish_date: Some(self.release.publish_date.clone()),
            indexer_source_id: self.release.indexer_source_id,
            decision: self.decision,
            reject_reasons: self.reject_reasons.clone(),
            match_score: self.match_score,
            matched_pattern: self.matched_pattern.clone(),
            matched_target: self.matched_target.clone(),
            quality_score: self.quality_score,
            total_score: Some(self.total_score),
            quality: self
                .parsed_quality
                .as_ref()
                .and_then(|pq| serde_json::to_value(pq).ok()),
            quality_rejected: self.quality_rejected,
            quality_reject_reasons: self.quality_reject_reasons.clone(),
            structural_rejected: self.structural_rejected,
            structural_reject_reason: self.structural_reject_reason,
            episode_key: self.episode_key.clone(),
            context_flags: self.context_flags.clone(),
        }
    }
}

impl Release {
    /// Convert a generic `SearchResult` into a `Release`.
    ///
    /// Used by backlog search and final-search-at-hold-expiry to work with
    /// the same `Release` type that RSS/watch handlers produce.
    pub fn from_search_result(result: &crate::types::SearchResult) -> Self {
        Release {
            guid: result.guid.clone().unwrap_or_else(|| result.id.clone()),
            title: result.title.clone(),
            file_name: result.file_name.clone(),
            download_url: result.download_url.clone(),
            magnet_url: result.magnet_url.clone(),
            info_url: result.info_url.clone(),
            size: result.size,
            publish_date: result.publish_date.clone().unwrap_or_default(),
            category_ids: result
                .categories
                .as_ref()
                .map(|cats| cats.iter().map(|c| c.id).collect())
                .unwrap_or_default(),
            indexer: result.indexer.clone(),
            protocol: result.protocol.clone(),
            info_hash: result.info_hash.clone(),
            files: result.files,
            seeders: result.torrent.as_ref().and_then(|t| t.seeders),
            peers: result.torrent.as_ref().and_then(|t| t.peers),
            leechers: result.torrent.as_ref().and_then(|t| t.leechers),
            grabs: result.usenet.as_ref().and_then(|u| u.grabs),
            votes: None,
            indexer_id: result.indexer_id.as_ref().and_then(|id| id.parse().ok()),
            indexer_source_id: None,
            raw_data: serde_json::json!(result),
            external_ids: Vec::new(),
            media: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_external_id_source_serde_round_trip() {
        let sources = vec![
            ExternalIdSource::Imdb,
            ExternalIdSource::Tmdb,
            ExternalIdSource::Tvdb,
            ExternalIdSource::TvMaze,
            ExternalIdSource::Isbn,
        ];

        for source in sources {
            let id = ExternalId {
                source: source.clone(),
                id: "test123".to_string(),
            };
            let json = serde_json::to_string(&id).expect("serialize");
            let deserialized: ExternalId = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(deserialized.source, source);
            assert_eq!(deserialized.id, "test123");
        }
    }

    #[test]
    fn test_release_media_serde_round_trip() {
        let variants = vec![
            ReleaseMedia::Tv {
                season: Some(2),
                episode: Some(5),
                year: Some(2020),
            },
            ReleaseMedia::Movie { year: Some(2024) },
            ReleaseMedia::Book {
                author: Some("Author".to_string()),
            },
            ReleaseMedia::Unknown,
        ];

        for media in variants {
            let json = serde_json::to_string(&media).expect("serialize");
            let deserialized: ReleaseMedia = serde_json::from_str(&json).expect("deserialize");
            let json2 = serde_json::to_string(&deserialized).expect("re-serialize");
            assert_eq!(json, json2);
        }
    }
}
