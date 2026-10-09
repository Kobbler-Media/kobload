use axum::http::StatusCode;
use serde::Serialize;
use sqlx::PgPool;
use tokio::sync::broadcast;
use uuid::Uuid;

use std::sync::Arc;

use crate::download_clients::DownloadClientResolver;
use crate::indexers::IndexerResolver;
use crate::nats::NatsState;
use crate::scene_mappings::SceneMappingCache;
use crate::trash_filters::TrashFilterSet;
use crate::types::DownloadStreamEvent;
use crate::watch::MatchTarget;
use crate::watch::ProcessedRelease;
use crate::watch::pattern_lock::PatternSearchLocks;
use crate::xem::XemCache;

#[derive(Clone)]
pub struct WantServiceContext<'a> {
    pub pool: &'a PgPool,
    pub http_client: &'a reqwest::Client,
    pub download_events: &'a broadcast::Sender<DownloadStreamEvent>,
    pub download_clients: &'a dyn DownloadClientResolver,
    pub indexers: &'a dyn IndexerResolver,
    pub xem_cache: &'a XemCache,
    pub scene_mappings: &'a SceneMappingCache,
    pub trash_filters: Option<Arc<TrashFilterSet>>,
    pub pattern_search_locks: &'a PatternSearchLocks,
    pub nats: &'a NatsState,
}

#[derive(Debug, Clone)]
pub struct WantServiceRequest {
    pub source_system: String,
    pub source_id: String,
    pub callback_url: String,
    pub auth_token: Option<String>,
    pub patterns: Vec<String>,
    pub match_targets: Vec<MatchTarget>,
    pub match_mode: String,
    pub min_score: f64,
    pub early_exit_score: f64,
    pub media_type_id: Option<Uuid>,
    pub auto_download: bool,
    pub limit: u32,
    pub metadata: Option<serde_json::Value>,
    pub media_want: Option<nats_jobs::MediaWant>,
    pub upgrade_allowed: bool,
    /// A person asked for this exact thing right now. Bypasses the
    /// upgrade-eligibility gate (and the already-covered fast path) so something
    /// already imported is a candidate again; the quality profile still picks.
    pub manual: bool,
    pub search_not_before: Option<time::OffsetDateTime>,
    /// Set by handle_want after creating the watch pattern; used by immediate search
    /// to create protocol-delay holds.
    pub watch_pattern_id: Option<Uuid>,
    /// Releases to exclude from matching (previously rejected/replaced).
    pub excluded_releases: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WantServiceResult {
    pub watch_pattern_id: Uuid,
    pub status: nats_jobs::WantStatus,
    pub download_id: Option<Uuid>,
    pub selected_release: Option<ProcessedRelease>,
    pub results_found: usize,
    pub best_score: f64,
    pub matched_pattern: Option<String>,
    pub matched_target: Option<nats_jobs::MatchedTarget>,
    /// For TvSeason: episode identifiers that actually got downloads.
    pub episodes_downloaded: Vec<String>,
}

#[derive(Debug)]
pub struct WantServiceError {
    pub status: StatusCode,
    pub message: String,
}

impl WantServiceError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for WantServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for WantServiceError {}
