//! File listing and content serving for watch directories.

use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use serde::Deserialize;
use tokio_util::io::ReaderStream;
use tracing::warn;
use uuid::Uuid;

use crate::auth::require_auth;
use crate::repositories::watch_directories as wd_repo;
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct ListFilesQuery {
    #[serde(default = "default_limit")]
    pub limit: i64,
    #[serde(default)]
    pub offset: i64,
}

fn default_limit() -> i64 {
    100
}

/// List files for a watch directory with pagination.
pub async fn list_files(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Query(query): Query<ListFilesQuery>,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    // Verify directory exists
    match wd_repo::get_by_id(&state.pool, id).await {
        Ok(Some(_)) => {}
        Ok(None) => return (StatusCode::NOT_FOUND, "directory not found").into_response(),
        Err(err) => {
            warn!(error = %err, "Failed to get watch directory");
            return (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response();
        }
    }

    match wd_repo::list_files(&state.pool, id, query.limit, query.offset).await {
        Ok(files) => Json(files).into_response(),
        Err(err) => {
            warn!(error = %err, "Failed to list watch directory files");
            (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response()
        }
    }
}

/// Stream a file's content from disk.
pub async fn serve_file_content(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((dir_id, file_id)): Path<(Uuid, Uuid)>,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    // Look up the file and verify it belongs to the correct directory
    let file = match wd_repo::get_file_by_id(&state.pool, file_id).await {
        Ok(Some(f)) => f,
        Ok(None) => return (StatusCode::NOT_FOUND, "file not found").into_response(),
        Err(err) => {
            warn!(error = %err, "Failed to look up watch directory file");
            return (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response();
        }
    };

    // Verify the file belongs to the requested directory (auth boundary)
    if file.watch_directory_id != dir_id {
        return (StatusCode::NOT_FOUND, "file not found").into_response();
    }

    // Open and stream the file
    let disk_file = match tokio::fs::File::open(&file.file_path).await {
        Ok(f) => f,
        Err(_) => return (StatusCode::NOT_FOUND, "file not found on disk").into_response(),
    };

    let stream = ReaderStream::new(disk_file);
    let body = axum::body::Body::from_stream(stream);
    (StatusCode::OK, body).into_response()
}
