use axum::{
    Json, Router, extract::State, http::HeaderMap, http::StatusCode, response::IntoResponse,
};
use serde::Serialize;

use crate::auth::require_auth;
use crate::repositories::watch as watch_repo;
use crate::state::AppState;

#[derive(Debug, Serialize)]
struct IndexerHealth {
    id: String,
    name: String,
    enabled: bool,
    consecutive_failures: i32,
    health_status: String,
    last_success_at: Option<String>,
    last_failure_at: Option<String>,
    last_error: Option<String>,
}

#[derive(Debug, Serialize)]
struct SystemHealthResponse {
    indexers: Vec<IndexerHealth>,
    active_patterns: i64,
    total_indexers: usize,
    healthy_indexers: usize,
    degraded_indexers: usize,
    unhealthy_indexers: usize,
}

fn health_status_label(failures: i32, auth_failed: bool) -> &'static str {
    if auth_failed {
        return "auth_failed";
    }
    match failures {
        0 => "healthy",
        1..=3 => "degraded",
        _ => "unhealthy",
    }
}

async fn get_system_health(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let sources = match watch_repo::list_indexer_sources(&state.pool).await {
        Ok(s) => s,
        Err(e) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
        }
    };

    let pattern_count = match watch_repo::list_patterns_by_enabled(&state.pool, true).await {
        Ok(p) => p.len() as i64,
        Err(_) => 0,
    };

    let mut healthy = 0usize;
    let mut degraded = 0usize;
    let mut unhealthy = 0usize;

    let indexers: Vec<IndexerHealth> = sources
        .iter()
        .map(|s| {
            let status = health_status_label(s.consecutive_failures, s.auth_failed_at.is_some());
            match status {
                "healthy" => healthy += 1,
                "degraded" => degraded += 1,
                _ => unhealthy += 1,
            }
            IndexerHealth {
                id: s.id.to_string(),
                name: s.name.clone(),
                enabled: s.enabled,
                consecutive_failures: s.consecutive_failures,
                health_status: status.to_string(),
                last_success_at: s.last_success_at.map(|t| t.to_string()),
                last_failure_at: s.last_failure_at.map(|t| t.to_string()),
                last_error: s.last_error.clone(),
            }
        })
        .collect();

    Json(SystemHealthResponse {
        total_indexers: indexers.len(),
        healthy_indexers: healthy,
        degraded_indexers: degraded,
        unhealthy_indexers: unhealthy,
        active_patterns: pattern_count,
        indexers,
    })
    .into_response()
}

pub fn routes() -> Router<AppState> {
    Router::new().route(
        "/api/v1/admin/system/health",
        axum::routing::get(get_system_health),
    )
}
