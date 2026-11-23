use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use usenet_proto::EngineStatus;

use crate::auth::require_auth;
use crate::state::AppState;

#[derive(Serialize)]
#[serde(tag = "engine", rename_all = "snake_case")]
enum UsenetStatusView {
    NotConfigured,
    Unavailable,
    Available { status: EngineStatus },
}

// Always 200 for an admin: this is a page decoration, and an engine that is down must not
// read as the Downloads page failing.
pub async fn status(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if let Err(code) = require_auth(&state, &headers).await {
        return code.into_response();
    }
    let view = match state.debrid_providers.usenet_engine() {
        None => UsenetStatusView::NotConfigured,
        Some(engine) => match engine.status().await {
            Ok(status) => UsenetStatusView::Available { status },
            Err(error) => {
                tracing::debug!(%error, "usenet engine status unavailable");
                UsenetStatusView::Unavailable
            }
        },
    };
    Json(view).into_response()
}
