use axum::Router;
use axum::routing::{get, post, put};

use crate::state::AppState;

mod engine_config;
mod servers;
mod status;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/admin/usenet/servers",
            get(servers::list).post(servers::create),
        )
        .route(
            "/api/v1/admin/usenet/servers/test",
            post(servers::test_unsaved),
        )
        .route(
            "/api/v1/admin/usenet/servers/{id}",
            put(servers::update)
                .get(servers::get)
                .delete(servers::delete),
        )
        .route(
            "/api/v1/admin/usenet/servers/{id}/test",
            post(servers::test_saved),
        )
        .route("/api/v1/admin/usenet/status", get(status::status))
        .route(
            "/api/v1/usenet/engine-config",
            get(engine_config::engine_config),
        )
}
