use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::auth::require_auth;
use crate::learned_aliases::{AliasScope, RememberError, scope_targets};
use crate::repositories::learned_aliases::LearnedAliasRow;
use crate::repositories::review as repo;
use crate::repositories::watch as watch_repo;
use crate::review::Detail;
use crate::review::actions::{self, ActionError};
use crate::state::AppState;

type ApiError = (StatusCode, String);

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/review", get(list_items))
        .route("/api/v1/review/count", get(open_count))
        .route("/api/v1/review/{id}/grab", post(grab))
        .route("/api/v1/review/{id}/not-it", post(not_it))
        .route("/api/v1/review/{id}/remember", post(remember))
        .route("/api/v1/review/{id}/dismiss", post(dismiss))
}

async fn authorize(state: &AppState, headers: &HeaderMap) -> Result<(), ApiError> {
    require_auth(state, headers)
        .await
        .map_err(|_| (StatusCode::UNAUTHORIZED, "Unauthorized".to_string()))
}

fn internal(e: impl ToString) -> ApiError {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

fn action_error(e: ActionError) -> ApiError {
    let status = match &e {
        ActionError::NotFound => StatusCode::NOT_FOUND,
        ActionError::Closed => StatusCode::CONFLICT,
        ActionError::UnknownRelease
        | ActionError::Remember(RememberError::ScopeNotApplicable(_)) => {
            StatusCode::UNPROCESSABLE_ENTITY
        }
        ActionError::Remember(RememberError::Empty) => StatusCode::UNPROCESSABLE_ENTITY,
        ActionError::GrabFailed => StatusCode::BAD_GATEWAY,
        ActionError::Remember(RememberError::Repository(_)) | ActionError::Repository(_) => {
            StatusCode::INTERNAL_SERVER_ERROR
        }
    };
    (status, e.to_string())
}

#[derive(Debug, Serialize)]
pub struct ScopeOption {
    pub scope: AliasScope,
    pub label: String,
}

#[derive(Debug, Serialize)]
pub struct ReviewItemResponse {
    pub id: Uuid,
    pub kind: String,
    pub watch_pattern_id: Uuid,
    pub source_system: String,
    pub source_id: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    pub detail: Detail,
    pub scopes: Vec<ScopeOption>,
}

#[derive(Debug, Serialize)]
pub struct ReviewList {
    pub items: Vec<ReviewItemResponse>,
}

async fn list_items(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<ReviewList>, ApiError> {
    authorize(&state, &headers).await?;
    let mut items = Vec::new();
    for row in repo::list_open(&state.pool).await.map_err(internal)? {
        let Some(pattern) = watch_repo::get_pattern_by_id(&state.pool, row.watch_pattern_id)
            .await
            .map_err(internal)?
        else {
            continue;
        };
        let Ok(detail) = serde_json::from_value::<Detail>(row.detail) else {
            continue;
        };
        let scopes = pattern
            .media_want
            .and_then(|v| serde_json::from_value::<nats_jobs::MediaWant>(v).ok())
            .map(|want| {
                scope_targets(&want)
                    .into_iter()
                    .map(|t| ScopeOption {
                        scope: t.scope,
                        label: t.label,
                    })
                    .collect()
            })
            .unwrap_or_default();
        items.push(ReviewItemResponse {
            id: row.id,
            kind: row.kind,
            watch_pattern_id: row.watch_pattern_id,
            source_system: pattern.source_system,
            source_id: pattern.source_id,
            created_at: row.created_at,
            detail,
            scopes,
        });
    }
    Ok(Json(ReviewList { items }))
}

#[derive(Debug, Serialize)]
pub struct OpenCount {
    pub open: i64,
}

async fn open_count(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<OpenCount>, ApiError> {
    authorize(&state, &headers).await?;
    let open = repo::open_count(&state.pool).await.map_err(internal)?;
    Ok(Json(OpenCount { open }))
}

#[derive(Debug, Deserialize)]
pub struct ReleaseRequest {
    pub release_key: String,
}

#[derive(Debug, Serialize)]
pub struct GrabResponse {
    pub download_id: Uuid,
}

async fn grab(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(request): Json<ReleaseRequest>,
) -> Result<Json<GrabResponse>, ApiError> {
    authorize(&state, &headers).await?;
    let download_id = actions::grab(&state, id, &request.release_key)
        .await
        .map_err(action_error)?;
    Ok(Json(GrabResponse { download_id }))
}

async fn not_it(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(request): Json<ReleaseRequest>,
) -> Result<StatusCode, ApiError> {
    authorize(&state, &headers).await?;
    actions::not_it(&state, id, &request.release_key)
        .await
        .map_err(action_error)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Debug, Deserialize)]
pub struct RememberRequest {
    pub scope: AliasScope,
    pub alias: String,
}

async fn remember(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(request): Json<RememberRequest>,
) -> Result<Json<LearnedAliasRow>, ApiError> {
    authorize(&state, &headers).await?;
    actions::remember_name(&state, id, request.scope, &request.alias)
        .await
        .map(Json)
        .map_err(action_error)
}

async fn dismiss(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    authorize(&state, &headers).await?;
    actions::dismiss(&state, id).await.map_err(action_error)?;
    Ok(StatusCode::NO_CONTENT)
}
