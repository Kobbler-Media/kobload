use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use reqwest::Client;
use sqlx::PgPool;
use tokio::sync::broadcast;

use crate::debrid::DebridProviderResolver;
use crate::download_clients::DownloadClientResolver;
use crate::indexers::IndexerResolver;
use crate::indexers::cardigann::DefinitionCache;
use crate::nats::NatsState;
use crate::rate_limit::RateLimiter;
use crate::scene_mappings::SceneMappingCache;
use crate::trash_filters::TrashFilterCache;
use crate::types::{DownloadStreamEvent, WatchPatternStreamEvent, WatchStreamEvent};
use crate::watch::pattern_lock::PatternSearchLocks;
use crate::xem::XemCache;

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub api_key: Option<String>,
    pub public_base_url: String,
    /// Path to unrar executable (for RAR archive extraction)
    pub unrar_path: Option<String>,
    /// Path to 7z executable (for 7z archive extraction)
    pub sevenz_path: Option<String>,
    /// Broadcast channel for download stream events
    pub download_events: broadcast::Sender<DownloadStreamEvent>,
    /// Broadcast channel for watch match stream events
    pub watch_events: broadcast::Sender<WatchStreamEvent>,
    /// Broadcast channel for watch pattern stream events
    pub watch_pattern_events: broadcast::Sender<WatchPatternStreamEvent>,
    /// NATS state for cross-service communication
    pub nats: NatsState,
    /// Shared HTTP client for external requests (uses connection pooling)
    pub http_client: Client,
    /// Debrid provider handler resolver (injectable for testing)
    pub debrid_providers: Arc<dyn DebridProviderResolver>,
    /// Download client handler resolver (injectable for testing)
    pub download_clients: Arc<dyn DownloadClientResolver>,
    /// Indexer handler resolver (injectable for testing)
    pub indexers: Arc<dyn IndexerResolver>,
    /// Cardigann YAML definition cache
    pub definitions: DefinitionCache,
    /// XEM scene number mapping cache
    pub xem_cache: Arc<XemCache>,
    /// Sonarr scene mapping cache (community-curated TV title aliases)
    pub scene_mappings: Arc<SceneMappingCache>,
    /// Rate limiter for login endpoint
    pub login_limiter: Arc<RateLimiter>,
    /// TRaSH Guides community filter cache (LQ groups, upscaled, tier bonuses)
    pub trash_filters: Arc<TrashFilterCache>,
    /// Set to true while backlog search is running; watch service skips its cycle.
    pub backlog_search_active: Arc<AtomicBool>,
    /// Per-pattern search+dispatch lock. Serialises want/backlog/RSS/manual/research
    /// on the same `watch_pattern_id` so they can't each dispatch a different release.
    pub pattern_search_locks: PatternSearchLocks,
}
