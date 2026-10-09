use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::get,
};
use serde::Deserialize;
use tracing::warn;
use uuid::Uuid;

use crate::auth::require_auth;
use crate::repositories::indexer_category_mappings as category_repo;
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct IndexerCategoryMappingCreate {
    pub indexer_source_id: Uuid,
    pub media_type_id: Uuid,
    pub indexer_category_id: i32,
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/admin/indexer-category-mappings/{source_id}",
            get(list_mappings_for_source).delete(delete_mappings_for_source),
        )
        .route(
            "/api/v1/admin/indexer-category-mappings",
            axum::routing::post(create_mapping),
        )
}

async fn list_mappings_for_source(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(source_id): Path<Uuid>,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    match category_repo::list_category_mappings_by_source(&state.pool, source_id).await {
        Ok(rows) => Json(rows).into_response(),
        Err(err) => {
            warn!(error = %err, "Failed to list category mappings");
            (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response()
        }
    }
}

async fn create_mapping(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<IndexerCategoryMappingCreate>,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let input = category_repo::IndexerCategoryMappingInput {
        indexer_source_id: payload.indexer_source_id,
        media_type_id: payload.media_type_id,
        indexer_category_id: payload.indexer_category_id,
    };

    match category_repo::create_category_mapping(&state.pool, input).await {
        Ok(id) => Json(serde_json::json!({ "id": id })).into_response(),
        Err(err) => {
            warn!(error = %err, "Failed to create category mapping");
            (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response()
        }
    }
}

async fn delete_mappings_for_source(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(source_id): Path<Uuid>,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    match category_repo::delete_category_mappings_by_source(&state.pool, source_id).await {
        Ok(_) => StatusCode::NO_CONTENT.into_response(),
        Err(err) => {
            warn!(error = %err, "Failed to delete category mappings");
            (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response()
        }
    }
}
