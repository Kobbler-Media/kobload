//! Admin routes for Kobload, including dev snapshot functionality.

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use serde::Serialize;
use std::path::Path;
use time::OffsetDateTime;

use crate::auth::require_auth;
use crate::backup;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/admin/dev/snapshot",
            get(get_dev_snapshot_status).delete(delete_dev_snapshot),
        )
        .route("/api/v1/admin/dev/snapshot/save", post(save_dev_snapshot))
}

// ========== Dev Snapshot (Dev Mode Only) ==========

#[derive(Serialize)]
pub struct DevSnapshotStatus {
    pub exists: bool,
    pub size: Option<u64>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub created_at: Option<OffsetDateTime>,
    pub dev_mode: bool,
    pub koblem_enabled: bool,
}

fn get_config_path() -> String {
    std::env::var("CONFIG_PATH").unwrap_or_else(|_| "/config".to_string())
}

fn get_dev_snapshot_path() -> std::path::PathBuf {
    Path::new(&get_config_path()).join("dev-snapshot.tar.gz")
}

fn is_dev_mode() -> bool {
    std::env::var("DEV_MODE")
        .ok()
        .filter(|s| s == "1")
        .is_some()
}

/// Koblem (local LLM review) is wired for one specific hardware setup and has
/// no non-DB way to edit its GPU gating, so its nav entry stays hidden unless
/// explicitly opted into.
fn is_koblem_ui_enabled() -> bool {
    std::env::var("KOBLEM_UI_ENABLED")
        .ok()
        .filter(|s| s == "1")
        .is_some()
}

/// Check if dev snapshot exists and get its status.
pub async fn get_dev_snapshot_status(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return (StatusCode::UNAUTHORIZED, "unauthorized").into_response();
    }

    let snapshot_path = get_dev_snapshot_path();
    let dev_mode = is_dev_mode();
    let koblem_enabled = is_koblem_ui_enabled();

    if !snapshot_path.exists() {
        return Json(DevSnapshotStatus {
            exists: false,
            size: None,
            created_at: None,
            dev_mode,
            koblem_enabled,
        })
        .into_response();
    }

    let metadata = match std::fs::metadata(&snapshot_path) {
        Ok(m) => m,
        Err(e) => {
            tracing::error!(error = %e, "Failed to get snapshot metadata");
            return Json(DevSnapshotStatus {
                exists: true,
                size: None,
                created_at: None,
                dev_mode,
                koblem_enabled,
            })
            .into_response();
        }
    };

    let created_at = metadata.modified().ok().and_then(|t| {
        let duration = t.duration_since(std::time::UNIX_EPOCH).ok()?;
        OffsetDateTime::from_unix_timestamp(duration.as_secs() as i64).ok()
    });

    Json(DevSnapshotStatus {
        exists: true,
        size: Some(metadata.len()),
        created_at,
        dev_mode,
        koblem_enabled,
    })
    .into_response()
}

/// Save current state as dev default snapshot.
pub async fn save_dev_snapshot(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return (StatusCode::UNAUTHORIZED, "unauthorized").into_response();
    }

    if !is_dev_mode() {
        return (StatusCode::FORBIDDEN, "dev mode not enabled").into_response();
    }

    let snapshot_path = get_dev_snapshot_path();

    // Ensure config directory exists
    if let Some(parent) = snapshot_path.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        tracing::error!(error = %e, "Failed to create config directory");
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("failed to create directory: {}", e),
        )
            .into_response();
    }

    match backup::create_backup(&state.pool, &snapshot_path).await {
        Ok(manifest) => Json(serde_json::json!({
            "success": true,
            "message": "Dev snapshot saved",
            "table_count": manifest.table_count
        }))
        .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "Failed to save dev snapshot");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to save snapshot: {}", e),
            )
                .into_response()
        }
    }
}

/// Delete the dev snapshot.
pub async fn delete_dev_snapshot(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return (StatusCode::UNAUTHORIZED, "unauthorized").into_response();
    }

    if !is_dev_mode() {
        return (StatusCode::FORBIDDEN, "dev mode not enabled").into_response();
    }

    let snapshot_path = get_dev_snapshot_path();

    if !snapshot_path.exists() {
        return (StatusCode::NOT_FOUND, "no dev snapshot exists").into_response();
    }

    match std::fs::remove_file(&snapshot_path) {
        Ok(_) => Json(serde_json::json!({
            "success": true,
            "message": "Dev snapshot deleted"
        }))
        .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "Failed to delete dev snapshot");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("failed to delete snapshot: {}", e),
            )
                .into_response()
        }
    }
}
