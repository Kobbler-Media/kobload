//! Serving files from disk to clients.

use axum::{
    body::Body,
    extract::Request,
    http::StatusCode,
    response::{IntoResponse, Response},
};
use tower_http::services::ServeFile;
use tracing::warn;

/// Serve a regular file with Range, HEAD and conditional-request support, so
/// players can seek and interrupted transfers can resume. `None` when `path`
/// is not a regular file on disk, letting callers fall back to archive
/// extraction for `archive!/inner` paths.
pub async fn serve_plain_file(path: &str, request: Request) -> Option<Response> {
    let is_file = tokio::fs::metadata(path)
        .await
        .map(|m| m.is_file())
        .unwrap_or(false);
    if !is_file {
        return None;
    }

    match ServeFile::new(path).try_call(request).await {
        Ok(response) => Some(response.map(Body::new)),
        Err(err) => {
            warn!(error = %err, path = %path, "Failed to serve file");
            Some((StatusCode::INTERNAL_SERVER_ERROR, "failed to read file").into_response())
        }
    }
}
