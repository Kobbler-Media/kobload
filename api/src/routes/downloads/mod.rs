mod crud;
mod lifecycle;
mod streaming;

use axum::{
    Router,
    routing::{get, patch, post},
};
use serde::{Deserialize, Serialize};

use crate::state::AppState;

pub use crud::*;
pub use lifecycle::*;
pub use streaming::*;

#[derive(Debug, Deserialize)]
pub struct StreamQuery {
    pub token: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct DeleteDownloadQuery {
    #[serde(default)]
    pub cancel: bool,
}

#[derive(Debug, Deserialize)]
pub struct ReingestRequest {
    pub callback_url: Option<String>,
    pub auth_token: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ReingestResponse {
    pub success: bool,
    pub message: String,
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/downloads",
            post(create_download)
                .get(list_downloads)
                .delete(clear_downloads),
        )
        .route(
            "/api/v1/downloads/{id}",
            get(get_download).delete(delete_download),
        )
        .route("/api/v1/downloads/{id}/events", get(get_download_events))
        .route("/api/v1/downloads/{id}/complete", post(complete_download))
        .route("/api/v1/downloads/{id}/imported", post(confirm_import))
        .route("/api/v1/downloads/{id}/import-failed", post(import_failed))
        .route(
            "/api/v1/downloads/{id}/import-deferred",
            post(import_deferred),
        )
        .route("/api/v1/downloads/{id}/reingest", post(reingest_download))
        .route("/api/v1/downloads/{id}/retry", patch(reingest_download))
        .route("/api/v1/downloads/{id}/rescan", post(rescan_download))
        .route("/api/v1/downloads/{id}/file", get(download_file))
        .route(
            "/api/v1/downloads/{id}/files/{file_id}",
            get(download_file_by_id),
        )
        .route("/api/v1/downloads/stream", get(stream_downloads))
}
