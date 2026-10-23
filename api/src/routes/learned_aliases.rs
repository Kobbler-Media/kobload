use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::{delete, get},
};
use serde::Deserialize;
use uuid::Uuid;

use crate::auth::require_auth;
use crate::learned_aliases::{AliasScope, RememberError, remember};
use crate::repositories::learned_aliases::{self as repo, LearnedAliasRow};
use crate::repositories::watch as watch_repo;
use crate::state::AppState;

type ApiError = (StatusCode, String);

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/learned-aliases",
            get(list_aliases).post(create_alias),
        )
        .route("/api/v1/learned-aliases/{id}", delete(delete_alias))
}

fn internal(e: impl ToString) -> ApiError {
    (StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

async fn authorize(state: &AppState, headers: &HeaderMap) -> Result<(), ApiError> {
    require_auth(state, headers)
        .await
        .map_err(|_| (StatusCode::UNAUTHORIZED, "Unauthorized".to_string()))
}

async fn list_aliases(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<LearnedAliasRow>>, ApiError> {
    authorize(&state, &headers).await?;
    repo::list(&state.pool).await.map(Json).map_err(internal)
}

#[derive(Debug, Deserialize)]
pub struct CreateAliasRequest {
    pub watch_pattern_id: Uuid,
    pub scope: AliasScope,
    pub alias: String,
}

async fn create_alias(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<CreateAliasRequest>,
) -> Result<Json<LearnedAliasRow>, ApiError> {
    authorize(&state, &headers).await?;
    let want = watch_repo::get_pattern_by_id(&state.pool, request.watch_pattern_id)
        .await
        .map_err(internal)?
        .and_then(|p| p.media_want)
        .and_then(|v| serde_json::from_value::<nats_jobs::MediaWant>(v).ok())
        .ok_or((
            StatusCode::NOT_FOUND,
            "No want for this pattern".to_string(),
        ))?;
    remember(&state.pool, &want, request.scope, &request.alias)
        .await
        .map(Json)
        .map_err(|e| match e {
            RememberError::Repository(e) => internal(e),
            e => (StatusCode::UNPROCESSABLE_ENTITY, e.to_string()),
        })
}

async fn delete_alias(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, ApiError> {
    authorize(&state, &headers).await?;
    match repo::delete(&state.pool, id).await.map_err(internal)? {
        true => Ok(StatusCode::NO_CONTENT),
        false => Err((StatusCode::NOT_FOUND, "Alias not found".to_string())),
    }
}
