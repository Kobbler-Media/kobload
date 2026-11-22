use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use tracing::warn;

use crate::state::AppState;
use crate::usenet::config;

/// The one place passwords leave kobload, so it takes the engine's own key and nothing else:
/// an admin session or API key is refused.
pub async fn engine_config(State(state): State<AppState>, headers: HeaderMap) -> Response {
    let Some(engine) = state.debrid_providers.usenet_engine() else {
        return (StatusCode::NOT_FOUND, "the usenet engine is not configured").into_response();
    };
    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default()
        .trim();
    if !engine.accepts_control_key(token) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match config::engine_config(&state.pool).await {
        Ok(config) => Json(config).into_response(),
        Err(e) => {
            warn!(error = %e, "Failed to load usenet servers for the engine");
            (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response()
        }
    }
}
