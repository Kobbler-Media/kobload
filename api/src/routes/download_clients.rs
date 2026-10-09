use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use serde::Deserialize;
use tracing::warn;
use uuid::Uuid;

use crate::auth::require_auth;
use crate::repositories::clients as clients_repo;
use crate::state::AppState;
use crate::types::{DownloadClientConfig, DownloadClientType, TestConnectionResult};

#[derive(Debug, Deserialize)]
pub struct DownloadClientCreate {
    name: String,
    client_type: String,
    base_url: String,
    api_key: Option<String>,
    username: Option<String>,
    password: Option<String>,
    category: Option<String>,
    download_dir: Option<String>,
    is_default: Option<bool>,
    delete_after_import: Option<bool>,
    seed_ratio: Option<f64>,
    seed_time_minutes: Option<i32>,
    enabled: Option<bool>,
}

#[derive(Debug, Deserialize)]
pub struct DownloadClientUpdate {
    name: Option<String>,
    base_url: Option<String>,
    api_key: Option<String>,
    username: Option<String>,
    password: Option<String>,
    category: Option<String>,
    download_dir: Option<String>,
    is_default: Option<bool>,
    delete_after_import: Option<bool>,
    seed_ratio: Option<f64>,
    seed_time_minutes: Option<i32>,
    enabled: Option<bool>,
}

#[derive(Debug, Deserialize, Default)]
pub struct TestDownloadClientRequest {
    client_type: Option<String>,
    base_url: Option<String>,
    api_key: Option<String>,
    username: Option<String>,
    password: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct TestDownloadClientConfigRequest {
    client_type: String,
    #[serde(default)]
    base_url: Option<String>,
    #[serde(default)]
    api_key: Option<String>,
    #[serde(default)]
    username: Option<String>,
    #[serde(default)]
    password: Option<String>,
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/admin/download-client-types",
            get(list_download_client_types),
        )
        .route(
            "/api/v1/admin/download-clients",
            get(list_download_clients).post(create_download_client),
        )
        .route(
            "/api/v1/admin/download-clients/test",
            post(test_download_client_config),
        )
        .route(
            "/api/v1/admin/download-clients/{id}",
            axum::routing::patch(update_download_client).delete(delete_download_client),
        )
        .route(
            "/api/v1/admin/download-clients/{id}/test",
            post(test_download_client_connection),
        )
}

async fn list_download_clients(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    match clients_repo::list_clients(&state.pool).await {
        Ok(download_clients) => Json(download_clients).into_response(),
        Err(err) => {
            warn!(error = %err, "Failed to load download clients");
            (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response()
        }
    }
}

async fn list_download_client_types(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    Json(DownloadClientType::all()).into_response()
}

async fn create_download_client(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<DownloadClientCreate>,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let client_type = payload.client_type.to_lowercase();

    // Look up the client type to get its protocol
    let client_type_info = match DownloadClientType::get(&client_type) {
        Some(info) => info,
        None => {
            return (StatusCode::BAD_REQUEST, "unsupported client_type").into_response();
        }
    };
    let protocol = client_type_info.protocol;

    let enabled = payload.enabled.unwrap_or(true);
    let is_default = payload.is_default.unwrap_or(false);
    let delete_after_import = payload.delete_after_import.unwrap_or(protocol == "usenet");

    let input = clients_repo::DownloadClientInput {
        name: payload.name,
        client_type,
        protocol: protocol.to_string(),
        base_url: payload.base_url,
        api_key: payload.api_key,
        username: payload.username,
        password: payload.password,
        category: payload.category,
        download_dir: payload.download_dir,
        is_default,
        delete_after_import,
        seed_ratio: payload.seed_ratio,
        seed_time_minutes: payload.seed_time_minutes,
        enabled,
    };

    match clients_repo::create_client(&state.pool, input).await {
        Ok(id) => {
            if is_default {
                let _ = clients_repo::clear_default_for_protocol_except(&state.pool, &protocol, id)
                    .await;
            }
            Json(serde_json::json!({ "id": id })).into_response()
        }
        Err(err) => {
            warn!(error = %err, "Failed to create download client");
            (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response()
        }
    }
}

async fn update_download_client(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(payload): Json<DownloadClientUpdate>,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let input = clients_repo::DownloadClientUpdate {
        name: payload.name,
        base_url: payload.base_url,
        api_key: payload.api_key,
        username: payload.username,
        password: payload.password,
        category: payload.category,
        download_dir: payload.download_dir,
        is_default: payload.is_default,
        delete_after_import: payload.delete_after_import,
        seed_ratio: payload.seed_ratio,
        seed_time_minutes: payload.seed_time_minutes,
        enabled: payload.enabled,
    };

    match clients_repo::update_client(&state.pool, id, input).await {
        Ok(true) => {
            if payload.is_default.unwrap_or(false)
                && let Ok(Some(protocol)) = clients_repo::get_client_protocol(&state.pool, id).await
            {
                let _ = clients_repo::clear_default_for_protocol_except(&state.pool, &protocol, id)
                    .await;
            }
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => (StatusCode::NOT_FOUND, "not found").into_response(),
        Err(err) => {
            warn!(error = %err, "Failed to update download client");
            (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response()
        }
    }
}

async fn delete_download_client(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    match clients_repo::delete_client(&state.pool, id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => (StatusCode::NOT_FOUND, "not found").into_response(),
        Err(err) => {
            warn!(error = %err, "Failed to delete download client");
            (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response()
        }
    }
}

async fn test_download_client_connection(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    body: Option<Json<TestDownloadClientRequest>>,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let db_client = match clients_repo::get_client_by_id(&state.pool, id).await {
        Ok(Some(client)) => client,
        Ok(None) => {
            return Json(TestConnectionResult {
                success: false,
                message: "Download client not found".to_string(),
            })
            .into_response();
        }
        Err(err) => {
            warn!(error = %err, "Failed to load download client");
            return Json(TestConnectionResult {
                success: false,
                message: "Database error".to_string(),
            })
            .into_response();
        }
    };

    // Merge POST body overrides with DB values
    let req = body.map(|b| b.0).unwrap_or_default();
    let client = DownloadClientConfig {
        client_type: req
            .client_type
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(db_client.client_type),
        base_url: req
            .base_url
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(db_client.base_url),
        api_key: req.api_key.or(db_client.api_key),
        username: req.username.or(db_client.username),
        password: req.password.or(db_client.password),
        ..db_client
    };

    let result = match state.download_clients.get(client.client_type.as_str()) {
        Some(handler) => handler.test_connection(&state.http_client, &client).await,
        None => TestConnectionResult {
            success: false,
            message: format!("Unknown client type: {}", client.client_type),
        },
    };

    Json(result).into_response()
}

async fn test_download_client_config(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<TestDownloadClientConfigRequest>,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    // Build a minimal DownloadClientConfig for testing
    let now = time::OffsetDateTime::now_utc();
    let client = DownloadClientConfig {
        id: Uuid::nil(),
        name: String::new(),
        client_type: req.client_type.clone(),
        protocol: String::new(),
        base_url: req.base_url.unwrap_or_default(),
        api_key: req.api_key,
        username: req.username,
        password: req.password,
        category: None,
        download_dir: None,
        is_default: false,
        delete_after_import: false,
        seed_ratio: None,
        seed_time_minutes: None,
        enabled: true,
        created_at: now,
        updated_at: now,
    };

    let result = match state.download_clients.get(client.client_type.as_str()) {
        Some(handler) => handler.test_connection(&state.http_client, &client).await,
        None => TestConnectionResult {
            success: false,
            message: format!("Unknown client type: {}", client.client_type),
        },
    };

    Json(result).into_response()
}
