use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use tracing::warn;

use crate::auth::require_auth;
use crate::helpers::load_prowlarr_config;
use crate::media::external_ids_from_media_want;
use crate::prowlarr::ProwlarrClient;
use crate::repositories::media_types as media_types_repo;
use crate::services::search::{SearchContext, SearchServiceContext, SearchServiceRequest, search};
use crate::state::AppState;
use crate::types::ProwlarrSearchRequest;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/search", post(generic_search))
        .route("/api/v1/prowlarr/search", post(prowlarr_search))
        .route("/api/v1/prowlarr/download-clients", get(prowlarr_clients))
}

async fn generic_search(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<nats_jobs::SearchRequest>,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let external_ids = payload
        .media_want
        .as_ref()
        .map(external_ids_from_media_want)
        .unwrap_or_default();

    // Resolve media_type from explicit key or derive from media_want
    let media_type_key = payload.media_type.as_deref().or_else(|| {
        payload
            .media_want
            .as_ref()
            .map(crate::media::media_type_key_from_media_want)
    });
    let media_type_id = if let Some(mt) = media_type_key {
        media_types_repo::get_by_key(&state.pool, mt)
            .await
            .ok()
            .flatten()
            .map(|row| row.id)
    } else {
        None
    };

    let service_request = SearchServiceRequest {
        query: payload.query.clone(),
        media_type_id,
        protocol: payload.protocol.clone(),
        limit: payload.limit,
        offset: payload.offset,
        media_want: payload.media_want.clone(),
        external_ids,
        search_context: SearchContext::Interactive,
    };

    let ctx = SearchServiceContext {
        pool: &state.pool,
        http_client: &state.http_client,
        indexers: state.indexers.as_ref(),
    };

    let result = match search(&ctx, service_request, None, None).await {
        Ok(result) => result,
        Err(err) => {
            return (
                err.status,
                Json(serde_json::json!({ "message": err.message })),
            )
                .into_response();
        }
    };

    Json(serde_json::json!({
        "results": result.results,
        "errors": result.errors,
    }))
    .into_response()
}

async fn prowlarr_search(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<ProwlarrSearchRequest>,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    // Resolve media_type from key string
    let media_type_id = if let Some(ref mt) = payload.media_type {
        media_types_repo::get_by_key(&state.pool, mt)
            .await
            .ok()
            .flatten()
            .map(|row| row.id)
    } else {
        None
    };

    let service_request = SearchServiceRequest {
        query: payload.q.clone(),
        media_type_id,
        protocol: payload.protocol.clone(),
        limit: None,
        offset: None,
        media_want: None,
        external_ids: Vec::new(),
        search_context: SearchContext::Interactive,
    };

    let ctx = SearchServiceContext {
        pool: &state.pool,
        http_client: &state.http_client,
        indexers: state.indexers.as_ref(),
    };

    let result = match search(&ctx, service_request, None, None).await {
        Ok(result) => result,
        Err(err) => {
            return (
                err.status,
                Json(serde_json::json!({ "message": err.message })),
            )
                .into_response();
        }
    };

    if result.results.is_empty() && !result.errors.is_empty() {
        return (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({ "message": result.errors.join("; ") })),
        )
            .into_response();
    }

    Json(serde_json::json!({
        "results": result.results,
        "errors": result.errors,
    }))
    .into_response()
}

async fn prowlarr_clients(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let config = match load_prowlarr_config(&state.pool).await {
        Ok(config) => config,
        Err(resp) => return resp,
    };
    let Some(base_url) = config.base_url else {
        return (StatusCode::BAD_REQUEST, "prowlarr_base_url not configured").into_response();
    };
    let Some(api_key) = config.api_key else {
        return (StatusCode::BAD_REQUEST, "prowlarr_api_key not configured").into_response();
    };

    let client = ProwlarrClient::new_with_client(base_url, api_key, state.http_client.clone());
    match client.download_clients().await {
        Ok(clients) => Json(clients).into_response(),
        Err(err) => {
            warn!(error = %err, "Prowlarr download clients failed");
            (StatusCode::BAD_GATEWAY, "prowlarr download clients failed").into_response()
        }
    }
}
