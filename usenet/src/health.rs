use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use kob_usenet::ReleaseKey;
use serde::Deserialize;
use usenet_proto::HealthSample;

use crate::error::ApiError;
use crate::state::AppState;

const DEFAULT_PERCENT: u8 = 5;

#[derive(Deserialize)]
pub struct Sample {
    percent: Option<u8>,
    output: Option<usize>,
}

pub async fn health(
    State(state): State<AppState>,
    Path(key): Path<String>,
    Query(sample): Query<Sample>,
) -> Result<Json<HealthSample>, ApiError> {
    let key: ReleaseKey = key
        .parse()
        .map_err(|_| ApiError::new(StatusCode::NOT_FOUND, "unknown_release"))?;
    let percent = sample.percent.unwrap_or(DEFAULT_PERCENT).clamp(1, 100);
    let sampled = state
        .engine
        .sample_health(&key, percent, sample.output)
        .await?;
    tracing::info!(%key, percent, output = ?sample.output, checked = sampled.checked, missing = sampled.missing, "health sampled");
    Ok(Json(HealthSample {
        checked: sampled.checked,
        missing: sampled.missing,
    }))
}
