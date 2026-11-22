use axum::Router;
use axum::extract::DefaultBodyLimit;
use axum::middleware;
use axum::routing::{get, post};

use crate::auth::require_bearer;
use crate::state::AppState;
use crate::{import, metrics, stream};

// Season-pack NZBs run to tens of MB, and base64 adds a third.
const IMPORT_LIMIT: usize = 128 << 20;

pub fn router(state: AppState) -> Router {
    let control = Router::new()
        .route(
            "/v1/import",
            post(import::import).layer(DefaultBodyLimit::max(IMPORT_LIMIT)),
        )
        .route("/v1/releases/{key}", get(import::release))
        .route("/metrics", get(metrics::metrics))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_bearer,
        ));
    Router::new()
        .route("/stream/{token}", get(stream::stream).head(stream::stream))
        .route("/healthz", get(|| async { "ok" }))
        .merge(control)
        .with_state(state)
}
