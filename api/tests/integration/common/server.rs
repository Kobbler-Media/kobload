use axum::{Json, Router, http::Method, response::IntoResponse, routing::get};
use kobload_api::{
    AppState,
    download_clients::{DefaultDownloadClientResolver, DownloadClientResolver},
    indexers::{DefaultIndexerResolver, IndexerResolver},
    nats::NatsState,
    routes, startup,
};
use reqwest::Client;
use serde_json::{Value, json};
use sqlx::{PgPool, postgres::PgPoolOptions};
use std::sync::Arc;
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};
use tokio::net::TcpListener;
use tokio::sync::{OnceCell, broadcast};
use tower_http::cors::{Any, CorsLayer};

use super::client::TestClient;

/// Get a unique run ID for this test run.
fn get_run_id() -> &'static str {
    static RUN_ID: OnceLock<String> = OnceLock::new();
    RUN_ID.get_or_init(|| {
        std::env::var("TEST_RUN_ID").unwrap_or_else(|_| {
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs();
            let pid = std::process::id();
            format!("{}_{}", timestamp, pid)
        })
    })
}

/// Get the template database name for this run.
fn template_db_name() -> String {
    format!("kobload_tpl_{}", get_run_id())
}

/// Get the prefix for test databases in this run.
fn test_db_prefix() -> String {
    format!("test_{}_", get_run_id())
}

/// Async-safe initialization of template database
static TEMPLATE_INIT: OnceCell<()> = OnceCell::const_new();

async fn init_test_env() {
    // Install rustls crypto provider (only needs to happen once, use std::sync::Once)
    static CRYPTO_INIT: std::sync::Once = std::sync::Once::new();
    CRYPTO_INIT.call_once(|| {
        rustls::crypto::ring::default_provider()
            .install_default()
            .expect("Failed to install ring crypto provider");

        // Initialize logging
        tracing_subscriber::fmt()
            .with_env_filter("kobload_api=debug,sqlx=warn,test=debug")
            .with_test_writer()
            .try_init()
            .ok();
    });

    // Initialize template database (async, runs once)
    TEMPLATE_INIT
        .get_or_init(|| async {
            init_template_database().await;
        })
        .await;
}

async fn connect(url: &str, max_connections: u32) -> Result<PgPool, sqlx::Error> {
    PgPoolOptions::new()
        .max_connections(max_connections)
        .connect(url)
        .await
}

async fn migrate(pool: &PgPool) -> Result<(), sqlx::migrate::MigrateError> {
    sqlx::migrate!().run(pool).await
}

/// Create or update the template database with migrations
async fn init_template_database() {
    let base_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://kobload:kobload@localhost:5434/kobload_test".to_string());

    // Parse to get connection to postgres database (for admin operations)
    let postgres_url = base_url
        .rsplit_once('/')
        .map(|(base, _)| format!("{}/postgres", base))
        .unwrap_or_else(|| base_url.clone());

    let admin_pool = connect(&postgres_url, 2)
        .await
        .expect("Failed to connect to postgres database");

    // Clean up stale databases from old test runs
    cleanup_stale_databases(&admin_pool).await;

    let template_name = template_db_name();

    // Drop and recreate template database to ensure clean state
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP DATABASE IF EXISTS {} WITH (FORCE)",
        template_name
    )))
    .execute(&admin_pool)
    .await
    .ok();

    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE DATABASE {}",
        template_name
    )))
    .execute(&admin_pool)
    .await
    .expect("Failed to create template database");

    // Connect to template and run migrations
    let template_url = base_url
        .rsplit_once('/')
        .map(|(base, _)| format!("{}/{}", base, template_name))
        .unwrap_or_else(|| base_url.clone());

    let template_pool = connect(&template_url, 2)
        .await
        .expect("Failed to connect to template database");

    migrate(&template_pool)
        .await
        .expect("Failed to run migrations on template");

    template_pool.close().await;
}

/// Clean up stale test databases older than 24 hours.
async fn cleanup_stale_databases(admin_pool: &PgPool) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let cutoff = now.saturating_sub(24 * 3600);

    let databases: Vec<String> = sqlx::query_scalar(
        "SELECT datname FROM pg_database WHERE datname LIKE 'test_%' OR datname LIKE 'kobload_tpl_%'",
    )
    .fetch_all(admin_pool)
    .await
    .unwrap_or_default();

    let current_run_id = get_run_id();

    for db in databases {
        if db.contains(current_run_id) {
            continue;
        }

        let timestamp = extract_timestamp_from_db_name(&db);
        if let Some(ts) = timestamp {
            if ts < cutoff {
                sqlx::query(sqlx::AssertSqlSafe(format!(
                    "DROP DATABASE IF EXISTS {} WITH (FORCE)",
                    db
                )))
                .execute(admin_pool)
                .await
                .ok();
            }
        }
    }
}

fn extract_timestamp_from_db_name(name: &str) -> Option<u64> {
    let parts: Vec<&str> = name.split('_').collect();
    if name.starts_with("test_") && parts.len() >= 4 {
        parts[1].parse().ok()
    } else if name.starts_with("kobload_tpl_") && parts.len() >= 4 {
        parts[2].parse().ok()
    } else {
        None
    }
}

async fn health() -> impl IntoResponse {
    Json(serde_json::json!({ "status": "ok" }))
}

/// A test server instance with pre-configured clients.
pub struct TestServer {
    /// Base URL for the API
    pub api_url: String,
    pub admin_token: String,
    pub pool: PgPool,
    /// App state for direct handler construction in tests
    pub app_state: AppState,
    /// Unique database name for this test
    db_name: String,
    _api_shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

/// Options for configuring the test server.
pub struct TestServerOptions {
    pub download_clients: Arc<dyn DownloadClientResolver>,
    pub indexers: Arc<dyn IndexerResolver>,
}

/// Third-party feeds (XEM, Sonarr scene mappings, TRaSH) fail fast and fall
/// back to the database, so tests seed what they need there and never depend
/// on the network.
pub fn local_only_http_client() -> reqwest::Client {
    reqwest::Client::builder()
        .proxy(
            reqwest::Proxy::all("http://127.0.0.1:9")
                .unwrap()
                .no_proxy(reqwest::NoProxy::from_string("127.0.0.1,localhost")),
        )
        .build()
        .unwrap()
}

impl TestServerOptions {
    pub fn with_pool(pool: &sqlx::PgPool) -> Self {
        Self {
            download_clients: Arc::new(DefaultDownloadClientResolver::new(
                pool.clone(),
                local_only_http_client(),
                std::sync::Arc::new(kobload_api::debrid::DefaultDebridProviderResolver),
            )),
            indexers: Arc::new(DefaultIndexerResolver::new(Default::default())),
        }
    }
}

impl Default for TestServerOptions {
    fn default() -> Self {
        // Fallback for tests that don't need debrid — uses a dummy pool
        // Tests using debrid should use TestServerOptions::with_pool()
        Self {
            download_clients: Arc::new(DefaultDownloadClientResolver::new(
                sqlx::PgPool::connect_lazy("postgres://dummy").unwrap(),
                local_only_http_client(),
                std::sync::Arc::new(kobload_api::debrid::DefaultDebridProviderResolver),
            )),
            indexers: Arc::new(DefaultIndexerResolver::new(Default::default())),
        }
    }
}

impl TestServer {
    /// Start a test server with default (real) resolvers.
    pub async fn start() -> Self {
        Self::start_with_options(TestServerOptions::default()).await
    }

    /// Start a test server with custom resolvers (for mock injection).
    pub async fn start_with_options(options: TestServerOptions) -> Self {
        init_test_env().await;

        let base_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://kobload:kobload@localhost:5434/kobload_test".to_string()
        });

        // Generate unique database name for this test
        let db_name = format!("{}{}", test_db_prefix(), uuid::Uuid::new_v4().simple());

        // Connect to postgres to create database from template
        let postgres_url = base_url
            .rsplit_once('/')
            .map(|(base, _)| format!("{}/postgres", base))
            .unwrap_or_else(|| base_url.clone());

        let admin_pool = connect(&postgres_url, 2)
            .await
            .expect("Failed to connect to postgres database");

        // Create database from template
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "CREATE DATABASE {} TEMPLATE {}",
            db_name,
            template_db_name()
        )))
        .execute(&admin_pool)
        .await
        .expect("Failed to create test database from template");

        // Connect to the new test database
        let database_url = base_url
            .rsplit_once('/')
            .map(|(base, _)| format!("{}/{}", base, db_name))
            .unwrap_or_else(|| base_url.clone());

        let pool = connect(&database_url, 5)
            .await
            .expect("Failed to connect to test database");

        // Find random available port
        let api_listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("Failed to bind to random port");
        let api_addr = api_listener.local_addr().unwrap();
        let api_url = format!("http://{}", api_addr);

        let (download_events, _) = broadcast::channel(256);
        let (watch_events, _) = broadcast::channel(64);
        let (watch_pattern_events, _) = broadcast::channel(64);

        let state = AppState {
            pool: pool.clone(),
            api_key: None,
            public_base_url: api_url.clone(),
            unrar_path: None,
            sevenz_path: None,
            download_events,
            watch_events,
            watch_pattern_events,
            nats: NatsState::new(None),
            http_client: local_only_http_client(),
            debrid_providers: std::sync::Arc::new(
                kobload_api::debrid::DefaultDebridProviderResolver,
            ),
            download_clients: options.download_clients,
            indexers: options.indexers,
            definitions: Default::default(),
            xem_cache: std::sync::Arc::new(kobload_api::xem::XemCache::new()),
            scene_mappings: std::sync::Arc::new(
                kobload_api::scene_mappings::SceneMappingCache::new(),
            ),
            login_limiter: std::sync::Arc::new(kobload_api::rate_limit::RateLimiter::new(
                10,
                std::time::Duration::from_secs(60),
            )),
            backlog_search_active: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            pattern_search_locks: kobload_api::watch::pattern_lock::PatternSearchLocks::new(),
            trash_filters: std::sync::Arc::new(kobload_api::trash_filters::TrashFilterCache::new()),
        };
        let app_state = state.clone();

        // Bootstrap default media types
        startup::ensure_default_media_types(&pool)
            .await
            .expect("Failed to ensure default media types");

        let cors = CorsLayer::new()
            .allow_origin(Any)
            .allow_methods([Method::GET, Method::POST, Method::PATCH, Method::DELETE])
            .allow_headers([
                axum::http::header::AUTHORIZATION,
                axum::http::header::CONTENT_TYPE,
            ]);

        let app = Router::new()
            .route("/health", get(health))
            .merge(routes::admin::routes())
            .merge(routes::auth::routes())
            .merge(routes::config::routes())
            .merge(routes::download_clients::routes())
            .merge(routes::downloads::routes())
            .merge(routes::media_types::routes())
            .merge(routes::providers::routes())
            .merge(routes::quality_profiles::routes())
            .merge(routes::review::routes())
            .merge(routes::indexer_category_mappings::routes())
            .merge(routes::indexer_rss_cache::routes())
            .merge(routes::learned_aliases::routes())
            .merge(routes::search::routes())
            .merge(routes::search_download::routes())
            .merge(routes::stream::routes())
            .merge(routes::want::routes())
            .merge(routes::watch::routes())
            .merge(routes::watch_directories::routes())
            .merge(routes::filesystem::routes())
            .with_state(state)
            .layer(cors);

        // Spawn API server
        let (api_shutdown_tx, api_shutdown_rx) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            axum::serve(api_listener, app)
                .with_graceful_shutdown(async {
                    api_shutdown_rx.await.ok();
                })
                .await
                .ok();
        });

        // Create admin user directly in DB
        let admin_password = "testadmin123";
        let password_hash = kobload_api::auth::hash_password(admin_password).unwrap();
        sqlx::query("INSERT INTO users (id, email, password_hash) VALUES ($1, $2, $3)")
            .bind(uuid::Uuid::new_v4())
            .bind("admin@test.local")
            .bind(&password_hash)
            .execute(&pool)
            .await
            .expect("Failed to create admin user");

        // Login to get admin token
        let client = Client::new();
        let resp = client
            .post(format!("{}/api/v1/admin/login", api_url))
            .json(&json!({
                "email": "admin@test.local",
                "password": admin_password
            }))
            .send()
            .await
            .expect("Login request failed");

        let login_resp: Value = resp.json().await.expect("Failed to parse login response");
        let admin_token = login_resp["token"]
            .as_str()
            .expect("No token in response")
            .to_string();

        Self {
            api_url,
            admin_token,
            pool,
            app_state,
            db_name,
            _api_shutdown: Some(api_shutdown_tx),
        }
    }

    /// Get a client for admin API requests.
    pub fn admin_client(&self) -> TestClient {
        TestClient::new(&self.api_url).with_auth(&self.admin_token)
    }

    /// Get a client for unauthenticated API requests.
    pub fn anon_client(&self) -> TestClient {
        TestClient::new(&self.api_url)
    }
}

impl TestServer {
    /// Cleanup test database.
    pub async fn cleanup(mut self) {
        if let Some(tx) = self._api_shutdown.take() {
            tx.send(()).ok();
        }

        self.pool.close().await;

        let base_url = std::env::var("DATABASE_URL").unwrap_or_else(|_| {
            "postgres://kobload:kobload@localhost:5434/kobload_test".to_string()
        });

        let postgres_url = base_url
            .rsplit_once('/')
            .map(|(base, _)| format!("{}/postgres", base))
            .unwrap_or_else(|| base_url.clone());

        if let Ok(admin_pool) = connect(&postgres_url, 1).await {
            sqlx::query(sqlx::AssertSqlSafe(format!(
                "DROP DATABASE IF EXISTS {} WITH (FORCE)",
                self.db_name
            )))
            .execute(&admin_pool)
            .await
            .ok();
        }
    }
}

impl Drop for TestServer {
    fn drop(&mut self) {
        if let Some(tx) = self._api_shutdown.take() {
            tx.send(()).ok();
        }
    }
}

// =============================================================================
// Test Helper Functions
// =============================================================================

/// Get or create a media type and return its ID.
pub async fn get_media_type_id(pool: &sqlx::PgPool, key: &str) -> uuid::Uuid {
    let row: (uuid::Uuid,) = sqlx::query_as("SELECT id FROM media_type_config WHERE key = $1")
        .bind(key)
        .fetch_one(pool)
        .await
        .expect("media type not found");
    row.0
}

/// Create a user via the admin API.
pub async fn create_user(server: &TestServer, email: &str, password: &str) -> uuid::Uuid {
    let resp = server
        .admin_client()
        .post_json(
            "/api/v1/admin/users",
            &json!({
                "email": email,
                "password": password,
            }),
        )
        .await;
    resp.assert_ok();
    let json: Value = resp.json();
    json["user_id"]
        .as_str()
        .expect("No user_id in response")
        .parse()
        .expect("Invalid UUID")
}

/// Create an API key via the admin API.
pub async fn create_api_key(server: &TestServer, name: &str) -> (uuid::Uuid, String) {
    let resp = server
        .admin_client()
        .post_json(
            "/api/v1/admin/api-keys",
            &json!({
                "name": name,
            }),
        )
        .await;
    resp.assert_ok();
    let json: Value = resp.json();
    let id = json["id"]
        .as_str()
        .expect("No id in response")
        .parse()
        .expect("Invalid UUID");
    let key = json["key"]
        .as_str()
        .expect("No key in response")
        .to_string();
    (id, key)
}
