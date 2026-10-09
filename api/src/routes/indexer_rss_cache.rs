use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::get,
};
use serde::Serialize;
use time::{Duration, OffsetDateTime};
use tracing::warn;
use uuid::Uuid;

use crate::auth::require_auth;
use crate::repositories::indexer_rss_cache as cache_repo;
use crate::state::AppState;

#[derive(Debug, Serialize)]
pub struct IndexerRssCacheInfo {
    pub indexer_source_id: Uuid,
    pub fetched_at: OffsetDateTime,
    pub ttl_seconds: i32,
    pub expires_at: OffsetDateTime,
    pub size_bytes: usize,
}

pub fn routes() -> Router<AppState> {
    Router::new().route(
        "/api/v1/admin/indexer-rss-cache/{source_id}",
        get(get_rss_cache_info),
    )
}

async fn get_rss_cache_info(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(source_id): Path<Uuid>,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    match cache_repo::get_rss_cache(&state.pool, source_id).await {
        Ok(Some(cache)) => {
            let expires_at = cache.fetched_at + Duration::seconds(cache.ttl_seconds as i64);
            let info = IndexerRssCacheInfo {
                indexer_source_id: cache.indexer_source_id,
                fetched_at: cache.fetched_at,
                ttl_seconds: cache.ttl_seconds,
                expires_at,
                size_bytes: cache.response_xml.len(),
            };
            Json(info).into_response()
        }
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(err) => {
            warn!(error = %err, "Failed to load RSS cache");
            (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response()
        }
    }
}
