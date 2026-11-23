use axum::Router;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::StatusCode;
use axum::middleware;
use axum::routing::{get, post};

use crate::auth::{admit_upload, refresh_config, require_control_key, require_metrics_key};
use crate::state::AppState;
use crate::{health, import, jobs, metrics, status, stream};

// Season-pack NZBs run to tens of MB, and base64 adds a third.
const IMPORT_LIMIT: usize = 128 << 20;

pub fn router(state: AppState) -> Router {
    let control = Router::new()
        .route(
            "/v1/import",
            post(import::import)
                .layer(DefaultBodyLimit::max(IMPORT_LIMIT))
                .layer(middleware::from_fn_with_state(state.clone(), admit_upload)),
        )
        .route("/v1/config/refresh", post(refresh_config))
        .route("/v1/releases/{key}", get(import::release))
        .route("/v1/releases/{key}/health", post(health::health))
        .route(
            "/v1/jobs",
            get(jobs::list).merge(
                post(jobs::add)
                    .layer(DefaultBodyLimit::max(IMPORT_LIMIT))
                    .layer(middleware::from_fn_with_state(state.clone(), admit_upload)),
            ),
        )
        .route("/v1/jobs/{id}", get(jobs::get).delete(jobs::remove))
        .route("/v1/status", get(status::status))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_control_key,
        ));
    let scrape = Router::new()
        .route("/metrics", get(metrics::metrics))
        .route_layer(middleware::from_fn_with_state(
            state.clone(),
            require_metrics_key,
        ));
    Router::new()
        .route("/stream/{token}", get(stream::stream).head(stream::stream))
        .route("/healthz", get(healthz))
        .merge(control)
        .merge(scrape)
        .with_state(state)
}

async fn healthz(State(state): State<AppState>) -> (StatusCode, &'static str) {
    if state.configured.is_set() {
        (StatusCode::OK, "ok")
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "waiting for config")
    }
}
