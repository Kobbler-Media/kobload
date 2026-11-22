use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use usenet_proto::keys_match;

use crate::state::AppState;

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
