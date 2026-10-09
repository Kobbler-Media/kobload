//! CRUD handlers for watch directories.

use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};
use tracing::warn;
use uuid::Uuid;

use crate::auth::require_auth;
use crate::jobs::{self, JobContext};
use crate::repositories::watch_directories as wd_repo;
use crate::state::AppState;

// =============================================================================
// Request / Response types
// =============================================================================

#[derive(Debug, Deserialize)]
pub struct CreateWatchDirectoryRequest {
    pub path: String,
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_true")]
    pub recursive: bool,
    #[serde(default = "default_media_type")]
    pub media_type: String,
    pub callback_url: String,
    pub auth_token: Option<String>,
}

fn default_true() -> bool {
    true
}
fn default_media_type() -> String {
    "book".to_string()
}

#[derive(Debug, Deserialize)]
pub struct UpdateWatchDirectoryRequest {
    pub name: Option<String>,
    pub path: Option<String>,
    pub enabled: Option<bool>,
    pub recursive: Option<bool>,
    pub media_type: Option<String>,
    pub callback_url: Option<String>,
    pub auth_token: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct WatchDirectoryWithStats {
    #[serde(flatten)]
    pub directory: wd_repo::WatchDirectory,
    pub file_counts: Vec<wd_repo::FileStatusCount>,
}

// =============================================================================
// Handlers
// =============================================================================

pub async fn list_watch_directories(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    match wd_repo::list_all(&state.pool).await {
        Ok(dirs) => Json(dirs).into_response(),
        Err(err) => {
            warn!(error = %err, "Failed to list watch directories");
            (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response()
        }
    }
}

pub async fn create_watch_directory(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<CreateWatchDirectoryRequest>,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    if payload.path.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "path is required").into_response();
    }
    if payload.callback_url.trim().is_empty() {
        return (StatusCode::BAD_REQUEST, "callback_url is required").into_response();
    }

    let valid_types = ["book", "tv", "movie"];
    if !valid_types.contains(&payload.media_type.as_str()) {
        return (
            StatusCode::BAD_REQUEST,
            "media_type must be book, tv, or movie",
        )
            .into_response();
    }

    let input = wd_repo::WatchDirectoryInput {
        path: payload.path,
        name: payload.name,
        enabled: payload.enabled,
        recursive: payload.recursive,
        media_type: payload.media_type,
        callback_url: payload.callback_url,
        auth_token: payload.auth_token,
    };

    match wd_repo::create(&state.pool, input).await {
        Ok(dir) => {
            // Enqueue initial scan
            if dir.enabled {
                let _ = enqueue_scan(&state, dir.id, &dir.name).await;
            }
            (StatusCode::CREATED, Json(dir)).into_response()
        }
        Err(err) => {
            let msg = err.to_string();
            if msg.contains("duplicate key") || msg.contains("unique") {
                return (
                    StatusCode::CONFLICT,
                    "a watch directory with this path already exists",
                )
                    .into_response();
            }
            warn!(error = %err, "Failed to create watch directory");
            (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response()
        }
    }
}

pub async fn get_watch_directory(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let dir = match wd_repo::get_by_id(&state.pool, id).await {
        Ok(Some(dir)) => dir,
        Ok(None) => return (StatusCode::NOT_FOUND, "not found").into_response(),
        Err(err) => {
            warn!(error = %err, "Failed to get watch directory");
            return (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response();
        }
    };

    let file_counts = wd_repo::file_status_counts(&state.pool, id)
        .await
        .unwrap_or_default();

    Json(WatchDirectoryWithStats {
        directory: dir,
        file_counts,
    })
    .into_response()
}

pub async fn update_watch_directory(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(payload): Json<UpdateWatchDirectoryRequest>,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    if let Some(ref mt) = payload.media_type {
        let valid_types = ["book", "tv", "movie"];
        if !valid_types.contains(&mt.as_str()) {
            return (
                StatusCode::BAD_REQUEST,
                "media_type must be book, tv, or movie",
            )
                .into_response();
        }
    }

    let input = wd_repo::WatchDirectoryUpdate {
        name: payload.name,
        path: payload.path,
        enabled: payload.enabled,
        recursive: payload.recursive,
        media_type: payload.media_type,
        callback_url: payload.callback_url,
        auth_token: payload.auth_token,
    };

    match wd_repo::update(&state.pool, id, input).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => (StatusCode::NOT_FOUND, "not found").into_response(),
        Err(err) => {
            warn!(error = %err, "Failed to update watch directory");
            (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response()
        }
    }
}

pub async fn delete_watch_directory(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    match wd_repo::delete(&state.pool, id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => (StatusCode::NOT_FOUND, "not found").into_response(),
        Err(err) => {
            warn!(error = %err, "Failed to delete watch directory");
            (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response()
        }
    }
}

pub async fn scan_watch_directory(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let dir = match wd_repo::get_by_id(&state.pool, id).await {
        Ok(Some(dir)) => dir,
        Ok(None) => return (StatusCode::NOT_FOUND, "not found").into_response(),
        Err(err) => {
            warn!(error = %err, "Failed to get watch directory for scan");
            return (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response();
        }
    };

    match enqueue_scan(&state, dir.id, &dir.name).await {
        Some(_) => StatusCode::ACCEPTED.into_response(),
        None => (StatusCode::SERVICE_UNAVAILABLE, "job system unavailable").into_response(),
    }
}

pub async fn retry_watch_directory(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let dir = match wd_repo::get_by_id(&state.pool, id).await {
        Ok(Some(dir)) => dir,
        Ok(None) => return (StatusCode::NOT_FOUND, "not found").into_response(),
        Err(err) => {
            warn!(error = %err, "Failed to get watch directory for retry");
            return (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response();
        }
    };

    if let Err(err) = wd_repo::clear_error(&state.pool, id).await {
        warn!(error = %err, "Failed to clear error for watch directory");
        return (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response();
    }

    let _ = enqueue_scan(&state, dir.id, &dir.name).await;
    StatusCode::ACCEPTED.into_response()
}

// =============================================================================
// Helpers
// =============================================================================

async fn enqueue_scan(state: &AppState, dir_id: Uuid, dir_name: &str) -> Option<Uuid> {
    jobs::enqueue_optional(
        &state.pool,
        &state.nats.publisher,
        "watch_directory_scan",
        serde_json::json!({ "watch_directory_id": dir_id }),
        JobContext::for_entity(
            "watch_directory",
            dir_id,
            format!("Scanning watch directory: {}", dir_name),
        ),
    )
    .await
}
