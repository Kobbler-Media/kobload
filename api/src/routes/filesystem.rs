//! Route for browsing server filesystem directories.

use axum::{
    Json, Router,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::get,
};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::auth::require_auth;
use crate::state::AppState;

#[derive(Deserialize)]
pub struct BrowseQuery {
    #[serde(default = "default_path")]
    pub path: String,
}

fn default_path() -> String {
    "/".to_string()
}

#[derive(Serialize)]
pub struct BrowseResponse {
    pub current_path: String,
    pub parent_path: Option<String>,
    pub directories: Vec<DirectoryEntry>,
}

#[derive(Serialize)]
pub struct DirectoryEntry {
    pub name: String,
    pub path: String,
}

pub fn routes() -> Router<AppState> {
    Router::new().route("/api/v1/filesystem/browse", get(browse_directories))
}

async fn browse_directories(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<BrowseQuery>,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let path = PathBuf::from(&query.path);

    let canonical = match path.canonicalize() {
        Ok(p) => p,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": format!("Invalid path: {}", e)})),
            )
                .into_response();
        }
    };

    if !canonical.is_dir() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "Path is not a directory"})),
        )
            .into_response();
    }

    let current_path = canonical.to_string_lossy().to_string();
    let parent_path = canonical.parent().map(|p| p.to_string_lossy().to_string());

    let mut directories = Vec::new();

    let entries = match std::fs::read_dir(&canonical) {
        Ok(e) => e,
        Err(e) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": format!("Cannot read directory: {}", e)})),
            )
                .into_response();
        }
    };

    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();

        if name.starts_with('.') {
            continue;
        }

        let entry_path = entry.path();
        let is_dir = match entry_path.metadata() {
            Ok(m) => m.is_dir(),
            Err(_) => continue,
        };

        if is_dir {
            directories.push(DirectoryEntry {
                path: entry_path.to_string_lossy().to_string(),
                name,
            });
        }
    }

    directories.sort_by_key(|a| a.name.to_lowercase());

    Json(BrowseResponse {
        current_path,
        parent_path,
        directories,
    })
    .into_response()
}
