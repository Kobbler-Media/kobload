use axum::{
    Json, Router,
    extract::{Multipart, Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use tracing::{info, warn};
use uuid::Uuid;

use crate::auth::require_auth;
use crate::debrid::{DebridAccountStatus, DebridProviderType};
use crate::jobs;
use crate::repositories::{hash_index, watch as watch_repo};
use crate::state::AppState;

#[derive(Debug, Serialize)]
pub struct DebridProviderRow {
    pub id: Uuid,
    pub name: String,
    pub provider_type: String,
    pub api_key: String,
    pub enabled: bool,
    pub priority: i32,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Deserialize)]
pub struct CreateDebridProvider {
    pub name: String,
    pub provider_type: String,
    pub api_key: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub priority: i32,
}

#[derive(Debug, Deserialize)]
pub struct UpdateDebridProvider {
    pub name: Option<String>,
    pub provider_type: Option<String>,
    pub api_key: Option<String>,
    pub enabled: Option<bool>,
    pub priority: Option<i32>,
}

#[derive(Debug, Deserialize)]
pub struct TestDebridProviderRequest {
    pub provider_type: String,
    pub api_key: String,
}

#[derive(Debug, Serialize)]
pub struct TestDebridProviderResult {
    pub success: bool,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub account: Option<DebridAccountStatus>,
}

fn default_true() -> bool {
    true
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/admin/debrid/providers",
            get(list_providers).post(create_provider),
        )
        .route(
            "/api/v1/admin/debrid/providers/test",
            post(test_provider_config),
        )
        .route(
            "/api/v1/admin/debrid/providers/{id}",
            axum::routing::put(update_provider).delete(delete_provider),
        )
        .route(
            "/api/v1/admin/debrid/providers/{id}/test",
            post(test_provider_connection),
        )
        .route(
            "/api/v1/admin/debrid/setup-public-trackers",
            post(setup_public_trackers),
        )
        .route(
            "/api/v1/admin/debrid/hash-index/import",
            post(hash_index_import),
        )
        .route(
            "/api/v1/admin/debrid/hash-index/stats",
            get(hash_index_stats),
        )
        .route(
            "/api/v1/admin/debrid/hash-index/bootstrap",
            post(hash_index_bootstrap),
        )
}

async fn list_providers(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    match sqlx::query_as!(
        DebridProviderRow,
        "SELECT id, name, provider_type, api_key, enabled, priority, created_at, updated_at
         FROM debrid_providers
         ORDER BY priority DESC, name"
    )
    .fetch_all(&state.pool)
    .await
    {
        Ok(rows) => Json(rows).into_response(),
        Err(err) => {
            warn!(error = %err, "Failed to list debrid providers");
            (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response()
        }
    }
}

async fn create_provider(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<CreateDebridProvider>,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    if DebridProviderType::parse(&payload.provider_type).is_none() {
        return (StatusCode::BAD_REQUEST, "unsupported provider_type").into_response();
    }

    match sqlx::query_scalar!(
        "INSERT INTO debrid_providers (name, provider_type, api_key, enabled, priority)
         VALUES ($1, $2, $3, $4, $5)
         RETURNING id",
        payload.name,
        payload.provider_type,
        payload.api_key,
        payload.enabled,
        payload.priority,
    )
    .fetch_one(&state.pool)
    .await
    {
        Ok(id) => Json(serde_json::json!({ "id": id })).into_response(),
        Err(err) => {
            warn!(error = %err, "Failed to create debrid provider");
            (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response()
        }
    }
}

async fn update_provider(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(payload): Json<UpdateDebridProvider>,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    if let Some(ref pt) = payload.provider_type
        && DebridProviderType::parse(pt).is_none()
    {
        return (StatusCode::BAD_REQUEST, "unsupported provider_type").into_response();
    }

    match sqlx::query!(
        "UPDATE debrid_providers
         SET name = COALESCE($2, name),
             provider_type = COALESCE($3, provider_type),
             api_key = COALESCE($4, api_key),
             enabled = COALESCE($5, enabled),
             priority = COALESCE($6, priority),
             updated_at = NOW()
         WHERE id = $1",
        id,
        payload.name,
        payload.provider_type,
        payload.api_key,
        payload.enabled,
        payload.priority,
    )
    .execute(&state.pool)
    .await
    {
        Ok(result) => {
            if result.rows_affected() == 0 {
                (StatusCode::NOT_FOUND, "not found").into_response()
            } else {
                StatusCode::NO_CONTENT.into_response()
            }
        }
        Err(err) => {
            warn!(error = %err, "Failed to update debrid provider");
            (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response()
        }
    }
}

async fn delete_provider(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    match sqlx::query!("DELETE FROM debrid_providers WHERE id = $1", id)
        .execute(&state.pool)
        .await
    {
        Ok(result) => {
            if result.rows_affected() == 0 {
                (StatusCode::NOT_FOUND, "not found").into_response()
            } else {
                StatusCode::NO_CONTENT.into_response()
            }
        }
        Err(err) => {
            warn!(error = %err, "Failed to delete debrid provider");
            (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response()
        }
    }
}

/// Test a debrid provider config before saving (no ID needed).
async fn test_provider_config(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<TestDebridProviderRequest>,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let handler = match state.debrid_providers.get_by_str(&req.provider_type) {
        Some(h) => h,
        None => {
            return Json(TestDebridProviderResult {
                success: false,
                message: format!("Unknown provider type: {}", req.provider_type),
                account: None,
            })
            .into_response();
        }
    };

    match handler
        .get_account_status(&state.http_client, &req.api_key)
        .await
    {
        Ok(status) => Json(TestDebridProviderResult {
            success: true,
            message: "Connection successful".to_string(),
            account: Some(status),
        })
        .into_response(),
        Err(err) => Json(TestDebridProviderResult {
            success: false,
            message: format!("Connection failed: {err}"),
            account: None,
        })
        .into_response(),
    }
}

/// Re-test an existing saved debrid provider.
async fn test_provider_connection(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let row = match sqlx::query!(
        "SELECT provider_type, api_key FROM debrid_providers WHERE id = $1",
        id
    )
    .fetch_optional(&state.pool)
    .await
    {
        Ok(Some(row)) => row,
        Ok(None) => {
            return Json(TestDebridProviderResult {
                success: false,
                message: "Debrid provider not found".to_string(),
                account: None,
            })
            .into_response();
        }
        Err(err) => {
            warn!(error = %err, "Failed to load debrid provider for test");
            return Json(TestDebridProviderResult {
                success: false,
                message: "Database error".to_string(),
                account: None,
            })
            .into_response();
        }
    };

    let handler = match state.debrid_providers.get_by_str(&row.provider_type) {
        Some(h) => h,
        None => {
            return Json(TestDebridProviderResult {
                success: false,
                message: format!("Unknown provider type: {}", row.provider_type),
                account: None,
            })
            .into_response();
        }
    };

    match handler
        .get_account_status(&state.http_client, &row.api_key)
        .await
    {
        Ok(status) => Json(TestDebridProviderResult {
            success: true,
            message: "Connection successful".to_string(),
            account: Some(status),
        })
        .into_response(),
        Err(err) => Json(TestDebridProviderResult {
            success: false,
            message: format!("Connection failed: {err}"),
            account: None,
        })
        .into_response(),
    }
}

/// Curated list of public tracker definition IDs and display names.
/// Definition IDs must match Prowlarr YAML filenames (without extension).
const PUBLIC_DEBRID_TRACKERS: &[(&str, &str)] = &[
    ("1337x", "1337x"),
    ("yts", "YTS"),
    ("eztv", "EZTV"),
    ("thepiratebay", "The Pirate Bay"),
    ("kickasstorrents-ws", "KickassTorrents"),
    ("torrentgalaxy", "TorrentGalaxy"),
    ("limetorrents", "LimeTorrents"),
    ("nyaasi", "Nyaa.si"),
    ("rutracker", "RuTracker"),
    ("animetosho", "Anime Tosho"),
    ("tokyotosho", "Tokyo Tosho"),
    ("magnetdl", "MagnetDL"),
    ("torrent9", "Torrent9"),
    ("ilcorsaronero", "ilCorSaRoNeRo"),
    ("bitsearch", "BitSearch"),
];

#[derive(Serialize)]
struct SetupPublicTrackersResponse {
    created: usize,
    skipped: usize,
    details: Vec<String>,
}

/// Auto-create indexer sources for popular public trackers with debrid_mode = 'always'.
async fn setup_public_trackers(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    if state.definitions.is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "No Cardigann definitions loaded. Download definitions first via Settings \u{2192} Definitions."
            })),
        )
            .into_response();
    }

    // Load existing cardigann sources to check for duplicates
    let existing = match watch_repo::list_indexer_sources_by_type(&state.pool, "cardigann").await {
        Ok(rows) => rows,
        Err(err) => {
            warn!(error = %err, "Failed to load existing indexer sources");
            return (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response();
        }
    };

    let existing_definitions: std::collections::HashSet<String> = existing
        .iter()
        .filter_map(|s| {
            s.handler_config
                .get("definition")
                .and_then(|v| v.as_str())
                .map(String::from)
        })
        .collect();

    let mut created = 0;
    let mut skipped = 0;
    let mut details = Vec::new();

    for &(def_id, display_name) in PUBLIC_DEBRID_TRACKERS {
        // Check if definition exists in the cache
        if !state.definitions.contains_key(def_id) {
            details.push(format!("{display_name}: definition not found, skipped"));
            skipped += 1;
            continue;
        }

        // Check if already exists
        if existing_definitions.contains(def_id) {
            details.push(format!("{display_name}: already exists, skipped"));
            skipped += 1;
            continue;
        }

        let handler_config = serde_json::json!({
            "definition": def_id,
            "settings": {},
            "poll_interval_minutes": 15
        });

        let input = watch_repo::IndexerSourceInput {
            name: display_name.to_string(),
            handler_type: "cardigann".to_string(),
            handler_config,
            category_ids: None,
            media_type_ids: vec![],
            enabled: true,
            priority: 0,
            seed_ratio: None,
            seed_time_minutes: None,
            search_mode: Some("full".to_string()),
            auto_throttle: Some(true),
            api_limit_daily: None,
            debrid_mode: Some("always".to_string()),
        };

        match watch_repo::create_indexer_source(&state.pool, input).await {
            Ok(_) => {
                details.push(format!("{display_name}: created"));
                created += 1;
            }
            Err(err) => {
                warn!(error = %err, name = display_name, "Failed to create public tracker source");
                details.push(format!("{display_name}: error - {err}"));
                skipped += 1;
            }
        }
    }

    info!(created, skipped, "Public tracker setup complete");
    Json(SetupPublicTrackersResponse {
        created,
        skipped,
        details,
    })
    .into_response()
}

// ---------------------------------------------------------------------------
// Hash Index
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct HashIndexImportRequest {
    source: String, // "rarbg", "tpb", "dmm", "all"
}

/// Trigger a hash index import job.
async fn hash_index_import(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<HashIndexImportRequest>,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let valid_sources = ["rarbg", "magnetdb", "tpb", "dmm", "all"];
    if !valid_sources.contains(&req.source.as_str()) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": format!("Invalid source: {}. Valid: {:?}", req.source, valid_sources) })),
        )
            .into_response();
    }

    // Check if an import is already running
    let already_running =
        jobs::has_active_job(&state.pool, "hash_index_import", "source", &req.source)
            .await
            .unwrap_or(false);

    if already_running {
        return (
            StatusCode::CONFLICT,
            Json(
                serde_json::json!({ "error": "An import job is already running for this source" }),
            ),
        )
            .into_response();
    }

    let payload = serde_json::json!({ "source": req.source });
    let context = nats_jobs::JobContext::new(format!("Hash index import: {}", req.source));

    match jobs::enqueue_optional(
        &state.pool,
        state.nats.publisher(),
        "hash_index_import",
        payload,
        context,
    )
    .await
    {
        Some(job_id) => Json(serde_json::json!({ "job_id": job_id })).into_response(),
        None => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "error": "NATS not available — cannot enqueue job" })),
        )
            .into_response(),
    }
}

/// Get hash index statistics.
async fn hash_index_stats(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let total = hash_index::count(&state.pool).await.unwrap_or(0);
    let by_source = hash_index::count_by_source(&state.pool)
        .await
        .unwrap_or_default();

    Json(serde_json::json!({
        "total": total,
        "by_source": by_source,
    }))
    .into_response()
}

/// Upload a pre-processed hash index TSV (gzipped) and enqueue a bootstrap import job.
async fn hash_index_bootstrap(
    State(state): State<AppState>,
    headers: HeaderMap,
    mut multipart: Multipart,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    // Check if a bootstrap is already running
    let already_running =
        jobs::has_active_job(&state.pool, "hash_index_bootstrap", "action", "bootstrap")
            .await
            .unwrap_or(false);

    if already_running {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({ "error": "A bootstrap import is already running" })),
        )
            .into_response();
    }

    // Stream the uploaded file directly to disk (don't buffer in memory)
    let upload_dir = std::path::PathBuf::from("data/hash-index-uploads");
    if let Err(e) = tokio::fs::create_dir_all(&upload_dir).await {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": format!("Failed to create upload directory: {e}") })),
        )
            .into_response();
    }

    let file_path = upload_dir.join("bootstrap.tsv.gz");
    let mut total_bytes = 0u64;
    let mut found_file = false;

    loop {
        let field = match multipart.next_field().await {
            Ok(Some(f)) => f,
            Ok(None) => break,
            Err(e) => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({ "error": format!("Multipart error: {e}") })),
                )
                    .into_response();
            }
        };

        if field.name() == Some("file") {
            found_file = true;
            // Stream chunks to file
            let mut file = match tokio::fs::File::create(&file_path).await {
                Ok(f) => f,
                Err(e) => {
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        Json(serde_json::json!({ "error": format!("Failed to create file: {e}") })),
                    )
                        .into_response();
                }
            };

            use tokio::io::AsyncWriteExt;
            let mut field = field;
            loop {
                match field.chunk().await {
                    Ok(Some(chunk)) => {
                        total_bytes += chunk.len() as u64;
                        if let Err(e) = file.write_all(&chunk).await {
                            return (
                                StatusCode::INTERNAL_SERVER_ERROR,
                                Json(serde_json::json!({ "error": format!("Write error: {e}") })),
                            )
                                .into_response();
                        }
                    }
                    Ok(None) => break, // Field complete
                    Err(e) => {
                        return (
                            StatusCode::BAD_REQUEST,
                            Json(serde_json::json!({ "error": format!("Upload error: {e}") })),
                        )
                            .into_response();
                    }
                }
            }
            file.flush().await.ok();
        }
    }

    if !found_file || total_bytes == 0 {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": "No file uploaded. Send a multipart form with a 'file' field containing the gzipped TSV." })),
        )
            .into_response();
    }

    let size_mb = total_bytes as f64 / 1024.0 / 1024.0;
    info!(
        size_mb = format!("{size_mb:.1}"),
        "Hash index bootstrap file uploaded (streamed)"
    );

    // Enqueue the bootstrap job
    let payload = serde_json::json!({
        "action": "bootstrap",
        "file_path": file_path.to_string_lossy(),
    });
    let context = nats_jobs::JobContext::new("Hash index bootstrap from uploaded file");

    match jobs::enqueue_optional(
        &state.pool,
        state.nats.publisher(),
        "hash_index_bootstrap",
        payload,
        context,
    )
    .await
    {
        Some(job_id) => Json(serde_json::json!({
            "job_id": job_id,
            "file_size_mb": format!("{size_mb:.1}"),
        }))
        .into_response(),
        None => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(serde_json::json!({ "error": "NATS not available — cannot enqueue job" })),
        )
            .into_response(),
    }
}
