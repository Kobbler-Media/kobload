use axum::body::{Body, HttpBody as _};
use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use usenet_proto::keys_match;

use crate::state::AppState;

const PEEK_LIMIT: usize = 16 << 10;

fn presented(request: &Request) -> Option<&str> {
    request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
}

async fn gate(expected: Option<&str>, request: Request, next: Next) -> Response {
    match (presented(&request), expected) {
        (Some(key), Some(expected)) if keys_match(key, expected) => next.run(request).await,
        _ => StatusCode::UNAUTHORIZED.into_response(),
    }
}

pub async fn require_control_key(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    gate(Some(&state.control_key), request, next).await
}

pub async fn require_metrics_key(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    gate(state.metrics_key.as_deref(), request, next).await
}

#[derive(Deserialize)]
struct UploadPeek {
    #[serde(default)]
    nzb_base64: Option<serde::de::IgnoredAny>,
}

fn declared_small(request: &Request) -> bool {
    request
        .body()
        .size_hint()
        .exact()
        .is_some_and(|len| len <= PEEK_LIMIT as u64)
}

// The slot is taken before a large body is read, so a waiting upload is still in the socket,
// not in memory. A small body is safe to read first, and one with no NZB is a cached lookup
// that must not queue behind slow imports.
pub async fn admit_upload(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let request = if declared_small(&request) {
        let (parts, body) = request.into_parts();
        let Ok(bytes) = axum::body::to_bytes(body, PEEK_LIMIT).await else {
            return StatusCode::BAD_REQUEST.into_response();
        };
        let cached = serde_json::from_slice::<UploadPeek>(&bytes)
            .is_ok_and(|peek| peek.nzb_base64.is_none());
        let request = Request::from_parts(parts, Body::from(bytes));
        if cached {
            return next.run(request).await;
        }
        request
    } else {
        request
    };
    let Ok(_permit) = state.uploads.acquire().await else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    next.run(request).await
}

pub async fn refresh_config(State(state): State<AppState>) -> StatusCode {
    state.config_refresh.notify_one();
    StatusCode::NO_CONTENT
}
