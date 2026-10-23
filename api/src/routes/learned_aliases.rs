use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::{delete, get},
};
use uuid::Uuid;

use crate::auth::require_auth;
use crate::repositories::learned_aliases::{self as repo, LearnedAliasRow};
use crate::state::AppState;

type ApiError = (StatusCode, String);

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/learned-aliases", get(list_aliases))
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
