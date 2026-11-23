use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use kob_usenet::ReleaseKey;
use serde::Deserialize;
use usenet_proto::HealthSample;

use crate::error::ApiError;
use crate::state::AppState;

#[derive(Deserialize)]
pub struct Sample {
    percent: Option<u8>,
    articles: Option<usize>,
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
    let sampled = match (sample.percent, sample.articles) {
        (Some(percent), None) => {
            state
                .engine
                .sample_health(&key, percent.clamp(1, 100), sample.output)
                .await?
        }
        (None, Some(articles)) if articles > 0 => {
            state
                .engine
                .quick_check(&key, articles, sample.output)
                .await?
        }
        _ => return Err(ApiError::new(StatusCode::BAD_REQUEST, "invalid_sample")),
    };
    tracing::info!(%key, percent = ?sample.percent, articles = ?sample.articles, output = ?sample.output, checked = sampled.checked, missing = sampled.missing, "health sampled");
    Ok(Json(HealthSample {
        checked: sampled.checked,
        missing: sampled.missing,
    }))
}
