use axum::http::HeaderValue;
use axum::{Json, Router, http::Method, response::IntoResponse, routing::get};
use sqlx::postgres::PgPoolOptions;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::broadcast;
use tower_http::cors::{Any, CorsLayer};
use tracing::{info, warn};

use kobload_api::{
    AppState, backup, decision_log,
    download_clients::DefaultDownloadClientResolver,
    indexers::DefaultIndexerResolver,
    nats,
    nats::NatsState,
    polling,
    rate_limit::RateLimiter,
    repositories::auth as auth_repo,
    routes, startup,
    types::{DownloadStreamEvent, WatchPatternStreamEvent, WatchStreamEvent},
    watch,
};

async fn health() -> impl IntoResponse {
    Json(serde_json::json!({ "status": "ok" }))
}

/// Panics on a malformed `KOBLOAD_CORS_ORIGIN` rather than silently falling back to
/// allow-all — a typo'd origin should fail loudly at startup, not open CORS wide.
fn build_cors_layer() -> CorsLayer {
    let base = CorsLayer::new()
        .allow_methods([Method::GET, Method::POST, Method::PATCH, Method::DELETE])
        .allow_headers([
            axum::http::header::AUTHORIZATION,
            axum::http::header::CONTENT_TYPE,
        ]);

    if let Some(origin) = std::env::var("KOBLOAD_CORS_ORIGIN")
        .ok()
        .filter(|s| !s.is_empty())
    {
        let val = HeaderValue::from_str(&origin)
            .unwrap_or_else(|e| panic!("Invalid KOBLOAD_CORS_ORIGIN value '{origin}': {e}"));
        base.allow_origin([val])
    } else {
        let is_dev = std::env::var("DEV_MODE")
            .ok()
            .filter(|s| s == "1")
            .is_some();
        if !is_dev {
            warn!(
                "KOBLOAD_CORS_ORIGIN not set — allowing all origins. Set it to your frontend URL in production."
            );
        }
        base.allow_origin(Any)
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenvy::dotenv().ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "info,kobload_api=info".to_string()),
        )
        .init();

    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://kobload:kobload@localhost/kobload".to_string());
    let api_key = std::env::var("KOBLOAD_API_KEY").ok();
    let admin_email =
        std::env::var("ADMIN_EMAIL").unwrap_or_else(|_| "admin@localhost".to_string());
    // Check ADMIN_PASSWORD first, fall back to DEV_ADMIN_PASSWORD for dev env consistency
    let admin_password = std::env::var("ADMIN_PASSWORD")
        .or_else(|_| std::env::var("DEV_ADMIN_PASSWORD"))
        .ok()
        .and_then(|value| {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        });
    let public_base_url =
        std::env::var("KOBLOAD_PUBLIC_URL").unwrap_or_else(|_| "http://localhost:7080".to_string());
    // Archive tool paths (for extracting rar/7z files)
    let unrar_path = std::env::var("UNRAR_PATH").ok().filter(|s| !s.is_empty());
    let sevenz_path = std::env::var("SEVENZ_PATH").ok().filter(|s| !s.is_empty());

    let pool = PgPoolOptions::new()
        .max_connections(100)
        .min_connections(5)
        .acquire_timeout(Duration::from_secs(10))
        .connect(&database_url)
        .await?;
    sqlx::migrate!().run(&pool).await?;

    // Dev mode: auto-restore from snapshot if DEV_AUTO_RESTORE=1 and snapshot exists
    // Only restores if database is fresh (no users yet)
    if std::env::var("DEV_AUTO_RESTORE")
        .ok()
        .filter(|s| s == "1")
        .is_some()
    {
        let config_path = std::env::var("CONFIG_PATH").unwrap_or_else(|_| "/config".to_string());
        let snapshot_path = Path::new(&config_path).join("dev-snapshot.tar.gz");

        if snapshot_path.exists() {
            // Check if database is fresh (no users = hasn't been bootstrapped or restored)
            let user_count = auth_repo::user_count(&pool).await.unwrap_or(0);

            if user_count == 0 {
                info!(
                    "Dev mode: fresh database detected, restoring from {:?}",
                    snapshot_path
                );
                match backup::restore_backup(&pool, &snapshot_path).await {
                    Ok(stats) => {
                        info!("Dev restore completed: {} tables", stats.tables_restored);
                    }
                    Err(e) => {
                        tracing::warn!("Dev restore failed (continuing): {}", e);
                    }
                }
            } else {
                info!(
                    "Dev mode: database already initialized ({} users), skipping restore",
                    user_count
                );
            }
        }
    }

    startup::bootstrap_admin(&pool, &admin_email, admin_password).await?;
    startup::ensure_default_media_types(&pool).await?;
    startup::ensure_default_quality_profiles(&pool).await?;

    let (download_events, _) = broadcast::channel::<DownloadStreamEvent>(256);
    let (watch_events, _) = broadcast::channel::<WatchStreamEvent>(64);
    let (watch_pattern_events, _) = broadcast::channel::<WatchPatternStreamEvent>(64);

    // Initialize NATS (optional - gracefully handles missing NATS server)
    let nats_client = nats::init_nats().await;
    let nats_state = NatsState::new(nats_client.clone());

    // Shared HTTP client for connection pooling
    let http_client = kobload_api::http::new_client();

    // Load Cardigann YAML definitions
    let definitions_dir = std::env::var("CARDIGANN_DEFINITIONS_DIR")
        .unwrap_or_else(|_| "/data/definitions".to_string());
    let definitions_override_dir = std::env::var("CARDIGANN_DEFINITIONS_OVERRIDE_DIR").ok();
    let definitions = kobload_api::indexers::cardigann::load_definitions(
        Path::new(&definitions_dir),
        definitions_override_dir.as_deref().map(Path::new),
    );

    // Auto-download definitions if none were found
    if definitions.is_empty() {
        info!("No cardigann definitions found, downloading from GitHub...");
        let dir = definitions_dir.clone();
        let dl_http = http_client.clone();
        match tokio::task::spawn_blocking(move || {
            kobload_api::routes::definitions::download_definitions(&dir, &dl_http)
        })
        .await
        {
            Ok(Ok(count)) => {
                kobload_api::indexers::cardigann::reload_definitions(
                    &definitions,
                    Path::new(&definitions_dir),
                    definitions_override_dir.as_deref().map(Path::new),
                );
                info!(
                    downloaded = count,
                    loaded = definitions.len(),
                    "Auto-downloaded cardigann definitions"
                );
            }
            Ok(Err(e)) => tracing::warn!("Failed to auto-download definitions: {e}"),
            Err(e) => tracing::warn!("Definition download task panicked: {e}"),
        }
    }

    let scene_mappings = Arc::new(kobload_api::scene_mappings::SceneMappingCache::new());
    scene_mappings.ensure_loaded(&http_client, &pool).await;

    let xem_cache = Arc::new(kobload_api::xem::XemCache::new());
    xem_cache.ensure_loaded(&http_client, &pool).await;

    let trash_filters = Arc::new(kobload_api::trash_filters::TrashFilterCache::new());
    trash_filters.ensure_loaded(&http_client, &pool).await;

    let download_clients: Arc<dyn kobload_api::download_clients::DownloadClientResolver> =
        Arc::new(DefaultDownloadClientResolver::new(
            pool.clone(),
            http_client.clone(),
            Arc::new(kobload_api::debrid::DefaultDebridProviderResolver),
        ));

    let state = AppState {
        pool,
        api_key,
        public_base_url,
        unrar_path,
        sevenz_path,
        download_events,
        watch_events,
        watch_pattern_events,
        nats: nats_state,
        http_client,
        debrid_providers: Arc::new(kobload_api::debrid::DefaultDebridProviderResolver),
        download_clients,
        indexers: Arc::new(DefaultIndexerResolver::new(definitions.clone())),
        definitions,
        xem_cache,
        scene_mappings,
        trash_filters,
        login_limiter: Arc::new(RateLimiter::new(10, Duration::from_secs(60))),
        backlog_search_active: Arc::new(std::sync::atomic::AtomicBool::new(false)),
        pattern_search_locks: kobload_api::watch::pattern_lock::PatternSearchLocks::new(),
    };
    let poll_state = state.clone();
    tokio::spawn(async move {
        polling::poll_downloads(poll_state).await;
    });

    // Spawn watch service for auto-watching patterns
    let watch_state = state.clone();
    tokio::spawn(async move {
        watch::run_watch_service(watch_state).await;
    });

    // Spawn decision log cleanup (runs daily, deletes old sessions)
    let cleanup_pool = state.pool.clone();
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(std::time::Duration::from_secs(24 * 3600)).await;
            match decision_log::repository::cleanup_old_sessions(&cleanup_pool, 7, 30).await {
                Ok(n) if n > 0 => tracing::info!(deleted = n, "Decision log cleanup complete"),
                Err(e) => tracing::warn!(error = %e, "Decision log cleanup failed"),
                _ => {}
            }
        }
    });

    // Start NATS job worker and scheduler (if NATS is available)
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    if let Some(nats) = nats_client {
        // Start the job worker
        let nats_clone = nats.clone();
        let worker_state = state.clone();
        let worker_shutdown_rx = shutdown_rx.clone();
        tokio::spawn(async move {
            nats::start_nats_worker(nats_clone, &worker_state, worker_shutdown_rx).await;
        });
        info!("NATS job worker started");

        // Start ingest-result subscription (receives import outcomes from kobbler)
        let ingest_state = state.clone();
        let ingest_nats = nats.clone();
        tokio::spawn(async move {
            nats::subscribe_ingest_result_events(ingest_nats, ingest_state).await;
        });
        info!("NATS ingest-result subscription started");

        // Start review-result subscription (receives LLM verdicts from koblem)
        let review_state = state.clone();
        let review_nats = nats.clone();
        tokio::spawn(async move {
            nats::subscribe_review_result_events(review_nats, review_state).await;
        });
        info!("NATS review-result subscription started");

        // Start the scheduler for periodic jobs
        let scheduler_shutdown_rx = shutdown_rx;
        tokio::spawn(async move {
            nats::start_scheduler(nats, scheduler_shutdown_rx).await;
        });
        info!("NATS scheduler started");

        // Check for stale/missing indexer caps and enqueue a refresh if needed
        {
            use kobload_api::jobs::{self, JobContext};
            use kobload_api::repositories::watch as watch_repo;
            let stale_count = watch_repo::count_stale_caps(&state.pool).await.unwrap_or(0);
            if stale_count > 0 {
                info!(
                    stale_count,
                    "Found indexer sources with stale/missing caps, enqueuing refresh"
                );
                let _ = jobs::enqueue_optional(
                    &state.pool,
                    &state.nats.publisher,
                    "refresh_indexer_caps",
                    serde_json::json!({}),
                    JobContext::new("Refresh stale indexer capabilities on startup"),
                )
                .await;
            }
        }
    }
    // Keep shutdown_tx alive for the duration of the server
    let _shutdown_tx = shutdown_tx;

    let cors = build_cors_layer();

    let app = Router::new()
        .route("/health", get(health))
        .merge(routes::admin::routes())
        .merge(routes::auth::routes())
        .merge(routes::config::routes())
        .merge(routes::dashboard::routes())
        .merge(routes::decision_logs::routes())
        .merge(routes::definitions::routes())
        .merge(routes::debrid::routes())
        .merge(routes::download_clients::routes())
        .merge(routes::downloads::routes())
        .merge(routes::filesystem::routes())
        .merge(routes::indexer_category_mappings::routes())
        .merge(routes::indexer_rss_cache::routes())
        .merge(routes::interpret::routes())
        .merge(routes::learned_aliases::routes())
        .merge(routes::media_types::routes())
        .merge(routes::providers::routes())
        .merge(routes::quality_profiles::routes())
        .merge(routes::review::routes())
        .merge(routes::search::routes())
        .merge(routes::search_download::routes())
        .merge(routes::stream::routes())
        .merge(routes::want::routes())
        .merge(routes::watch::routes())
        .merge(routes::watch_directories::routes())
        .merge(routes::system_health::routes())
        .merge(routes::webhooks::routes())
        .merge(routes::api_info::routes())
        .merge(kobload_api::openapi::routes())
        .with_state(state)
        .layer(axum::extract::DefaultBodyLimit::max(2 * 1024 * 1024 * 1024)) // 2GB — needed for hash index bootstrap upload
        .layer(cors);

    let addr = SocketAddr::from(([0, 0, 0, 0], 7080));
    info!("Kobload API listening on {}", addr);
    axum::serve(tokio::net::TcpListener::bind(addr).await?, app).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::build_cors_layer;

    // KOBLOAD_CORS_ORIGIN is process-global env, so this test owns it exclusively —
    // keep it the only test in this module touching that var.
    #[test]
    fn invalid_cors_origin_panics_instead_of_falling_back() {
        unsafe {
            std::env::set_var("KOBLOAD_CORS_ORIGIN", "not\na valid\nheader value");
        }
        let result = std::panic::catch_unwind(build_cors_layer);
        unsafe {
            std::env::remove_var("KOBLOAD_CORS_ORIGIN");
        }
        assert!(
            result.is_err(),
            "expected build_cors_layer to panic on an invalid origin value"
        );
    }
}
