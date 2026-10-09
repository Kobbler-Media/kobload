use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{
        IntoResponse,
        sse::{Event, KeepAlive, Sse},
    },
};
use serde::Deserialize;
use std::convert::Infallible;
use std::time::Duration;
use tokio_stream::{StreamExt, wrappers::BroadcastStream};
use uuid::Uuid;

use crate::auth::require_auth;
use crate::auth::require_auth_with_token;
use crate::repositories::watch as watch_repo;
use crate::state::AppState;
use crate::types::WatchStreamEvent;
use crate::watch::{ListWatchMatchesResponse, WatchMatchResponse};

fn default_match_limit() -> i64 {
    50
}

#[derive(Debug, Deserialize)]
pub(super) struct ListMatchesQuery {
    watch_pattern_id: Option<Uuid>,
    #[serde(default = "default_match_limit")]
    limit: i64,
    cursor: Option<String>,
    action: Option<String>,
    rejected: Option<bool>,
    search: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(super) struct StreamQuery {
    pub token: Option<String>,
}

/// List watch matches with cursor pagination
pub async fn list_watch_matches(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ListMatchesQuery>,
) -> Result<Json<ListWatchMatchesResponse>, (StatusCode, String)> {
    if require_auth(&state, &headers).await.is_err() {
        return Err((StatusCode::UNAUTHORIZED, "Unauthorized".to_string()));
    }

    let (cursor_ts, cursor_id) = query
        .cursor
        .as_ref()
        .and_then(|c| watch_repo::decode_match_cursor(c))
        .unzip();

    let options = watch_repo::WatchMatchListOptions {
        limit: query.limit,
        cursor: cursor_ts,
        cursor_id,
        watch_pattern_id: query.watch_pattern_id,
        action: query.action,
        rejected: query.rejected,
        search: query.search,
    };

    let list_response = watch_repo::fetch_match_list(&state.pool, options)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let items: Vec<WatchMatchResponse> = list_response.items.into_iter().map(Into::into).collect();

    Ok(Json(ListWatchMatchesResponse {
        items,
        next_cursor: list_response.next_cursor,
    }))
}

/// SSE stream for watch matches - sends snapshot on connect, then live updates.
pub async fn stream_watch_matches(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<StreamQuery>,
) -> impl IntoResponse {
    if require_auth_with_token(&state, &headers, query.token.as_deref())
        .await
        .is_err()
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    // Fetch initial snapshot of recent matches
    let initial_matches: Vec<WatchMatchResponse> =
        match watch_repo::list_matches(&state.pool, 100).await {
            Ok(rows) => rows.into_iter().map(Into::into).collect(),
            Err(err) => {
                tracing::warn!(error = %err, "Failed to fetch initial watch matches for stream");
                return StatusCode::INTERNAL_SERVER_ERROR.into_response();
            }
        };

    let initial_event = WatchStreamEvent::Snapshot {
        matches: initial_matches,
    };

    let rx = state.watch_events.subscribe();

    let initial_stream = tokio_stream::once(Ok::<_, Infallible>(sse_event_from_watch_event(
        initial_event,
    )));

    let broadcast_stream = BroadcastStream::new(rx).filter_map(|result| match result {
        Ok(event) => Some(Ok::<_, Infallible>(sse_event_from_watch_event(event))),
        Err(_) => None,
    });

    let combined_stream = initial_stream.chain(broadcast_stream);

    Sse::new(combined_stream)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(30)))
        .into_response()
}

fn sse_event_from_watch_event(event: WatchStreamEvent) -> Event {
    let payload = serde_json::to_string(&event).unwrap_or_else(|_| "{}".to_string());
    Event::default().data(payload)
}

#[derive(Debug, Deserialize)]
pub(super) struct BulkRejectRequest {
    pub ids: Vec<Uuid>,
    pub reason: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub(super) struct BulkRejectResponse {
    pub rejected: u64,
}

/// Bulk reject watch matches.
pub async fn bulk_reject_watch_matches(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<BulkRejectRequest>,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    match watch_repo::bulk_reject_matches(&state.pool, &body.ids, body.reason.as_deref()).await {
        Ok(rejected) => Json(BulkRejectResponse { rejected }).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "Failed to bulk reject watch matches");
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
        }
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct BulkDeleteMatchesRequest {
    pub ids: Vec<Uuid>,
}

#[derive(Debug, serde::Serialize)]
pub(super) struct BulkDeleteResponse {
    pub deleted: u64,
}

/// Bulk delete watch matches.
pub async fn bulk_delete_watch_matches(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<BulkDeleteMatchesRequest>,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    match watch_repo::bulk_delete_matches(&state.pool, &body.ids).await {
        Ok(deleted) => Json(BulkDeleteResponse { deleted }).into_response(),
        Err(e) => {
            tracing::error!(error = %e, "Failed to bulk delete watch matches");
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
        }
    }
}

#[derive(Debug, Deserialize)]
pub(super) struct RejectMatchRequest {
    pub reason: Option<String>,
}

/// Reject a watch match.
pub async fn reject_watch_match(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(match_id): Path<Uuid>,
    Json(body): Json<RejectMatchRequest>,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    match watch_repo::reject_match(&state.pool, match_id, body.reason.as_deref()).await {
        Ok(found) => {
            if found {
                StatusCode::NO_CONTENT.into_response()
            } else {
                StatusCode::NOT_FOUND.into_response()
            }
        }
        Err(e) => {
            tracing::error!(error = %e, %match_id, "Failed to reject watch match");
            (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
        }
    }
}
