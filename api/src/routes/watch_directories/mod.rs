//! Watch directory management routes.

mod crud;
mod files;

use axum::{Router, routing::get};

use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/watch-directories",
            get(crud::list_watch_directories).post(crud::create_watch_directory),
        )
        .route(
            "/api/v1/watch-directories/{id}",
            get(crud::get_watch_directory)
                .patch(crud::update_watch_directory)
                .delete(crud::delete_watch_directory),
        )
        .route(
            "/api/v1/watch-directories/{id}/scan",
            axum::routing::post(crud::scan_watch_directory),
        )
        .route(
            "/api/v1/watch-directories/{id}/retry",
            axum::routing::post(crud::retry_watch_directory),
        )
        .route(
            "/api/v1/watch-directories/{id}/files",
            get(files::list_files),
        )
        .route(
            "/api/v1/watch-directories/{dir_id}/files/{file_id}/content",
            get(files::serve_file_content),
        )
}
