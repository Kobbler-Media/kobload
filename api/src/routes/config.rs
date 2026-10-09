use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use serde::{Deserialize, Serialize};

use crate::auth::require_auth;
use crate::indexers::IndexerContext;
use crate::prowlarr::ProwlarrClient;
use crate::repositories::{app_settings, clients as clients_repo};
use crate::state::AppState;
use crate::types::{DownloadClientConfig, TestConnectionResult};

/// Known handler types with their display labels.
const HANDLER_TYPES: &[(&str, &str)] = &[
    ("prowlarr", "Prowlarr"),
    ("newznab", "Newznab"),
    ("torznab", "Torznab"),
    ("cardigann", "Cardigann"),
    ("mam", "MyAnonaMouse"),
    ("torrentday", "TorrentDay"),
];

#[derive(Debug, Deserialize)]
pub struct TestProwlarrRequest {
    base_url: String,
    api_key: String,
}

#[derive(Debug, Serialize)]
struct ConfigResponse {
    download_clients: Vec<DownloadClientConfig>,
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/admin/config", get(get_config))
        .route(
            "/api/v1/admin/test-prowlarr",
            post(test_prowlarr_connection),
        )
        .route("/api/v1/admin/indexer-handlers", get(list_indexer_handlers))
        .route("/api/v1/admin/test-indexer", post(test_indexer_connection))
        .route(
            "/api/v1/admin/protocol-defaults",
            get(get_protocol_defaults).put(set_protocol_defaults),
        )
        .route(
            "/api/v1/settings/upgrade-defaults",
            get(get_upgrade_defaults).put(set_upgrade_defaults),
        )
        .route(
            "/api/v1/settings/koblem",
            get(get_koblem_settings).put(set_koblem_settings),
        )
        .route("/api/v1/settings/koblem/test", post(test_koblem_connection))
}

async fn get_config(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    match clients_repo::list_clients(&state.pool).await {
        Ok(download_clients) => Json(ConfigResponse { download_clients }).into_response(),
        Err(err) => {
            tracing::warn!(error = %err, "Failed to load config");
            (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response()
        }
    }
}

async fn list_indexer_handlers(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let mut handlers = Vec::new();
    for &(type_id, label) in HANDLER_TYPES {
        let config_schema = state
            .indexers
            .get(type_id)
            .map(|h| h.config_schema())
            .unwrap_or_default();

        handlers.push(serde_json::json!({
            "id": type_id,
            "name": label,
            "config_schema": config_schema,
        }));
    }

    Json(handlers).into_response()
}

async fn get_protocol_defaults(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    match app_settings::get_protocol_defaults(&state.pool).await {
        Ok(defaults) => Json(defaults).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn set_protocol_defaults(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<app_settings::ProtocolDefaults>,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    match app_settings::set_protocol_defaults(&state.pool, &body).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn get_upgrade_defaults(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    match app_settings::get_upgrade_defaults(&state.pool).await {
        Ok(defaults) => Json(defaults).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn set_upgrade_defaults(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<app_settings::UpgradeDefaults>,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    match app_settings::set_upgrade_defaults(&state.pool, &body).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Debug, Deserialize)]
struct TestIndexerRequest {
    handler_type: String,
    handler_config: serde_json::Value,
}

async fn test_indexer_connection(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<TestIndexerRequest>,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let handler = match state.indexers.get(&req.handler_type) {
        Some(h) => h,
        None => {
            return Json(serde_json::json!({
                "success": false,
                "message": format!("Unknown handler type: {}", req.handler_type)
            }))
            .into_response();
        }
    };

    let ctx = IndexerContext {
        pool: &state.pool,
        http_client: &state.http_client,
    };

    match handler.test_connection(&ctx, &req.handler_config).await {
        Ok(msg) => Json(serde_json::json!({ "success": true, "message": msg })).into_response(),
        Err(e) => {
            Json(serde_json::json!({ "success": false, "message": e.message })).into_response()
        }
    }
}

async fn get_koblem_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    match app_settings::get_koblem_settings(&state.pool).await {
        Ok(settings) => Json(settings).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn set_koblem_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<app_settings::KoblemSettings>,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    match app_settings::set_koblem_settings(&state.pool, &body).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Debug, Deserialize)]
struct TestKoblemRequest {
    service_url: String,
}

async fn test_koblem_connection(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<TestKoblemRequest>,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    if req.service_url.trim().is_empty() {
        return Json(serde_json::json!({
            "success": false,
            "message": "Service URL is required"
        }))
        .into_response();
    }

    let client = koblem_client::KoblemClient::new(req.service_url);
    match client.health().await {
        Ok(health) => {
            let message = if health.llama_server {
                "Connected to koblem, LLM server is ready"
            } else {
                "Connected to koblem, but LLM server is not ready"
            };
            Json(serde_json::json!({
                "success": true,
                "message": message,
                "llama_server": health.llama_server,
            }))
            .into_response()
        }
        Err(e) => Json(serde_json::json!({
            "success": false,
            "message": format!("Connection failed: {e}"),
        }))
        .into_response(),
    }
}

async fn test_prowlarr_connection(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<TestProwlarrRequest>,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    if req.base_url.trim().is_empty() {
        return Json(TestConnectionResult {
            success: false,
            message: "Prowlarr base URL is required".to_string(),
        })
        .into_response();
    }

    if req.api_key.trim().is_empty() {
        return Json(TestConnectionResult {
            success: false,
            message: "Prowlarr API key is required".to_string(),
        })
        .into_response();
    }

    let client =
        ProwlarrClient::new_with_client(req.base_url, req.api_key, state.http_client.clone());
    match client.download_clients().await {
        Ok(_) => Json(TestConnectionResult {
            success: true,
            message: "Connected to Prowlarr successfully".to_string(),
        })
        .into_response(),
        Err(err) => Json(TestConnectionResult {
            success: false,
            message: format!("Connection failed: {err}"),
        })
        .into_response(),
    }
}
