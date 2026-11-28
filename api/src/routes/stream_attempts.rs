use std::convert::Infallible;
use std::time::Duration;

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{
        IntoResponse, Response,
        sse::{Event, KeepAlive, Sse},
    },
    routing::get,
};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use tokio_stream::{StreamExt, wrappers::BroadcastStream};
use uuid::Uuid;

use crate::auth::{require_auth, require_auth_with_token};
use crate::repositories::stream_attempts::{
    self as repo, ListFilter, StreamAttempt, StreamAttemptEvent,
};
use crate::repositories::stream_searches::{self, StreamSearch};
use crate::services::stream::attempt_log::{self, AttemptStreamEvent};
use crate::state::AppState;

const SNAPSHOT_SIZE: i64 = 100;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/stream-attempts",
            get(list_attempts).delete(clear_attempts),
        )
        .route("/api/v1/stream-attempts/stream", get(stream_attempts))
        .route("/api/v1/stream-attempts/{id}/events", get(attempt_events))
        .route("/api/v1/stream-searches", get(list_groups))
}

type ApiError = (StatusCode, String);

fn internal(e: impl ToString) -> ApiError {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

async fn authorize(state: &AppState, headers: &HeaderMap) -> Result<(), ApiError> {
    require_auth(state, headers)
        .await
        .map_err(|_| (StatusCode::UNAUTHORIZED, "Unauthorized".to_string()))
}

#[derive(Debug, Deserialize)]
struct ListQuery {
    status: Option<String>,
    search: Option<String>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    before: Option<OffsetDateTime>,
    limit: Option<i64>,
}

async fn list_attempts(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ListQuery>,
) -> Result<Json<Vec<StreamAttempt>>, ApiError> {
    authorize(&state, &headers).await?;
    let filter = ListFilter {
        members: None,
        status: query.status.as_deref().filter(|s| !s.is_empty()),
        search: query.search.as_deref().filter(|s| !s.is_empty()),
        before: query.before,
        limit: query.limit.unwrap_or(50).clamp(1, 200),
    };
    repo::list(&state.pool, &filter)
        .await
        .map(Json)
        .map_err(internal)
}

async fn attempt_events(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<Vec<StreamAttemptEvent>>, ApiError> {
    authorize(&state, &headers).await?;
    repo::events(&state.pool, id)
        .await
        .map(Json)
        .map_err(internal)
}

#[derive(Serialize)]
struct Cleared {
    deleted: u64,
}

async fn clear_attempts(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Cleared>, ApiError> {
    authorize(&state, &headers).await?;
    let deleted = repo::clear(&state.pool).await.map_err(internal)?;
    stream_searches::clear(&state.pool)
        .await
        .map_err(internal)?;
    Ok(Json(Cleared { deleted }))
}

/// A search with the attempts on its releases, or an attempt no search listed.
#[derive(Serialize)]
struct Group {
    id: Uuid,
    search: Option<StreamSearch>,
    attempts: Vec<StreamAttempt>,
}

#[derive(Debug, Deserialize)]
struct GroupQuery {
    search: Option<String>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    before: Option<OffsetDateTime>,
    limit: Option<i64>,
}

const MAX_GROUP_ATTEMPTS: i64 = 2000;

async fn list_groups(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<GroupQuery>,
) -> Result<Json<Vec<Group>>, ApiError> {
    authorize(&state, &headers).await?;
    let heads = repo::heads(
        &state.pool,
        query.search.as_deref().filter(|s| !s.is_empty()),
        query.before,
        query.limit.unwrap_or(30).clamp(1, 100),
    )
    .await
    .map_err(internal)?;
    let (search_ids, lone_ids): (Vec<_>, Vec<_>) = heads.iter().partition(|h| h.is_search);
    let search_ids: Vec<Uuid> = search_ids.iter().map(|h| h.id).collect();
    let lone_ids: Vec<Uuid> = lone_ids.iter().map(|h| h.id).collect();
    let mut searches = stream_searches::get_many(&state.pool, &search_ids)
        .await
        .map_err(internal)?;
    let filter = ListFilter {
        members: Some((&lone_ids, &search_ids)),
        limit: MAX_GROUP_ATTEMPTS,
        ..Default::default()
    };
    let mut attempts = repo::list(&state.pool, &filter).await.map_err(internal)?;
    let groups = heads
        .iter()
        .filter_map(|head| {
            if head.is_search {
                let at = searches.iter().position(|s| s.id == head.id)?;
                let search = searches.swap_remove(at);
                let (mine, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut attempts)
                    .into_iter()
                    .partition(|a| a.search_id == Some(head.id));
                attempts = rest;
                Some(Group {
                    id: head.id,
                    search: Some(search),
                    attempts: mine,
                })
            } else {
                let at = attempts.iter().position(|a| a.id == head.id)?;
                Some(Group {
                    id: head.id,
                    search: None,
                    attempts: vec![attempts.swap_remove(at)],
                })
            }
        })
        .collect();
    Ok(Json(groups))
}

#[derive(Debug, Deserialize)]
struct StreamQuery {
    token: Option<String>,
}

async fn stream_attempts(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<StreamQuery>,
) -> Response {
    if require_auth_with_token(&state, &headers, query.token.as_deref())
        .await
        .is_err()
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let rx = attempt_log::subscribe();
    let filter = ListFilter {
        limit: SNAPSHOT_SIZE,
        ..Default::default()
    };
    let attempts = match repo::list(&state.pool, &filter).await {
        Ok(a) => a,
        Err(e) => return internal(e).into_response(),
    };
    let snapshot = tokio_stream::once(AttemptStreamEvent::Snapshot { attempts });
    let updates = BroadcastStream::new(rx).filter_map(Result::ok);
    let events = snapshot.chain(updates).map(|event| {
        let payload = serde_json::to_string(&event).unwrap_or_else(|_| "{}".to_string());
        Ok::<_, Infallible>(Event::default().data(payload))
    });
    Sse::new(events)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(30)))
        .into_response()
}
