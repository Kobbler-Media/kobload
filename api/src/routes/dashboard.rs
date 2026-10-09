use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::get,
};
use serde::Serialize;

use crate::auth::require_auth;
use crate::repositories::{downloads as downloads_repo, watch as watch_repo};
use crate::state::AppState;
use crate::types::DownloadListItem;
use crate::watch::{WatchMatchResponse, WatchPatternResponse};

#[derive(Debug, Serialize)]
pub struct DashboardSummary {
    pub recent_downloads: Vec<DownloadListItem>,
    pub recent_matches: Vec<WatchMatchResponse>,
    pub recent_patterns: Vec<WatchPatternResponse>,
}

async fn get_dashboard_summary(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let (downloads_result, matches_result, patterns_result) = tokio::join!(
        downloads_repo::fetch_download_list(
            &state.pool,
            downloads_repo::DownloadListOptions {
                limit: 5,
                ..Default::default()
            },
        ),
        watch_repo::fetch_match_list(
            &state.pool,
            watch_repo::WatchMatchListOptions {
                limit: 5,
                ..Default::default()
            },
        ),
        watch_repo::fetch_pattern_list(
            &state.pool,
            watch_repo::WatchPatternListOptions {
                limit: 5,
                ..Default::default()
            },
        ),
    );

    let recent_downloads = match downloads_result {
        Ok(resp) => resp.items,
        Err(err) => {
            tracing::warn!(error = %err, "Failed to fetch recent downloads for dashboard");
            return (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response();
        }
    };

    let recent_matches = match matches_result {
        Ok(resp) => resp
            .items
            .into_iter()
            .map(WatchMatchResponse::from)
            .collect(),
        Err(err) => {
            tracing::warn!(error = %err, "Failed to fetch recent matches for dashboard");
            return (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response();
        }
    };

    let recent_patterns = match patterns_result {
        Ok(resp) => resp
            .items
            .into_iter()
            .map(WatchPatternResponse::from)
            .collect(),
        Err(err) => {
            tracing::warn!(error = %err, "Failed to fetch recent patterns for dashboard");
            return (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response();
        }
    };

    Json(DashboardSummary {
        recent_downloads,
        recent_matches,
        recent_patterns,
    })
    .into_response()
}

pub fn routes() -> Router<AppState> {
    Router::new().route(
        "/api/v1/admin/dashboard/summary",
        get(get_dashboard_summary),
    )
}
