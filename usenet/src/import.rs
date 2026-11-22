use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use kob_usenet::{Release, ReleaseKey};
use usenet_proto::{ImportMode, ImportRequest, ImportResponse, OutputInfo};

use crate::error::ApiError;
use crate::state::AppState;

pub async fn import(
    State(state): State<AppState>,
    Json(request): Json<ImportRequest>,
) -> Result<Json<ImportResponse>, ApiError> {
    let nzb = request
        .nzb_base64
        .as_deref()
        .map(|b64| STANDARD.decode(b64))
        .transpose()
        .map_err(|e| {
            ApiError::new(StatusCode::BAD_REQUEST, "invalid_base64").with_message(e.to_string())
        })?;
    let mode = match request.mode {
        ImportMode::Stream => kob_usenet::ImportMode::Stream,
        ImportMode::Prewarm => kob_usenet::ImportMode::Prewarm,
        ImportMode::Download => kob_usenet::ImportMode::Download,
    };
    let release = state
        .engine
        .import(
            nzb.as_deref(),
            Some(&request.cache_key),
            &request.name,
            request.password.as_deref(),
            mode,
        )
        .await?;
    tracing::info!(key = %release.key, cache_key = request.cache_key, outputs = release.outputs.len(), "imported");
    Ok(Json(describe(release)))
}

pub async fn release(
    State(state): State<AppState>,
    Path(key): Path<String>,
) -> Result<Json<ImportResponse>, ApiError> {
    let key: ReleaseKey = key
        .parse()
        .map_err(|_| ApiError::new(StatusCode::NOT_FOUND, "unknown_release"))?;
    Ok(Json(describe(state.engine.release(&key)?)))
}

fn describe(release: Release) -> ImportResponse {
    ImportResponse {
        release_key: release.key.to_string(),
        outputs: release
            .outputs
            .into_iter()
            .map(|o| OutputInfo {
                index: o.index,
                path: o.path,
                size: o.size,
            })
            .collect(),
    }
}
