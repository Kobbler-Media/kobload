use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::get,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::require_auth;
use crate::decision_log::repository;
use crate::decision_log::types::{DecisionLogSettings, SessionDetail, SessionExport};
use crate::repositories::app_settings;
use crate::state::AppState;

fn default_limit() -> i64 {
    50
}

#[derive(Debug, Deserialize)]
pub struct SessionListQuery {
    trigger_type: Option<String>,
    is_test: Option<bool>,
    watch_pattern_id: Option<Uuid>,
    #[serde(default = "default_limit")]
    limit: i64,
    #[serde(default)]
    offset: i64,
}

#[derive(Debug, Deserialize)]
pub struct BulkDeleteQuery {
    trigger_type: String,
}

#[derive(Debug, Serialize)]
struct BulkDeleteResponse {
    deleted: u64,
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/decision-logs/sessions",
            get(list_sessions).delete(bulk_delete_sessions),
        )
        .route(
            "/api/v1/decision-logs/sessions/{id}",
            get(get_session).delete(delete_session),
        )
        .route(
            "/api/v1/decision-logs/sessions/{id}/export",
            get(export_session),
        )
        .route(
            "/api/v1/decision-logs/settings",
            get(get_settings).put(update_settings),
        )
}

async fn list_sessions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<SessionListQuery>,
) -> Result<Json<Vec<crate::decision_log::types::SessionListItem>>, (StatusCode, String)> {
    if require_auth(&state, &headers).await.is_err() {
        return Err((StatusCode::UNAUTHORIZED, "Unauthorized".to_string()));
    }

    let trigger_types: Option<Vec<String>> = query
        .trigger_type
        .as_ref()
        .map(|t| t.split(',').map(|s| s.trim().to_string()).collect());
    let trigger_refs: Option<Vec<&str>> = trigger_types
        .as_ref()
        .map(|v| v.iter().map(String::as_str).collect());

    let sessions = repository::list_sessions(
        &state.pool,
        trigger_refs.as_deref(),
        query.is_test,
        query.watch_pattern_id,
        query.limit,
        query.offset,
    )
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(sessions))
}

async fn get_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<SessionDetail>, (StatusCode, String)> {
    if require_auth(&state, &headers).await.is_err() {
        return Err((StatusCode::UNAUTHORIZED, "Unauthorized".to_string()));
    }

    let session = repository::get_session_detail(&state.pool, id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, "Session not found".to_string()))?;

    Ok(Json(session))
}

async fn export_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<SessionExport>, (StatusCode, String)> {
    if require_auth(&state, &headers).await.is_err() {
        return Err((StatusCode::UNAUTHORIZED, "Unauthorized".to_string()));
    }

    let export = repository::get_session_export(&state.pool, id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, "Session not found".to_string()))?;

    Ok(Json(export))
}

async fn delete_session(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    match repository::delete_session(&state.pool, id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => (StatusCode::NOT_FOUND, "Session not found").into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn bulk_delete_sessions(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<BulkDeleteQuery>,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let types: Vec<&str> = query.trigger_type.split(',').map(str::trim).collect();

    match repository::delete_sessions_bulk(&state.pool, &types).await {
        Ok(deleted) => Json(BulkDeleteResponse { deleted }).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn get_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<DecisionLogSettings>, (StatusCode, String)> {
    if require_auth(&state, &headers).await.is_err() {
        return Err((StatusCode::UNAUTHORIZED, "Unauthorized".to_string()));
    }

    let search_logging_enabled = app_settings::get_search_logging_enabled(&state.pool)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let feed_logging_enabled = app_settings::get_feed_logging_enabled(&state.pool)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let debrid_logging_enabled = app_settings::get_debrid_logging_enabled(&state.pool)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(DecisionLogSettings {
        search_logging_enabled,
        feed_logging_enabled,
        debrid_logging_enabled,
    }))
}

#[derive(Debug, Deserialize)]
struct UpdateSettingsBody {
    search_logging_enabled: Option<bool>,
    feed_logging_enabled: Option<bool>,
    debrid_logging_enabled: Option<bool>,
}

async fn update_settings(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<UpdateSettingsBody>,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    if let Some(val) = body.search_logging_enabled
        && let Err(e) = app_settings::set(
            &state.pool,
            app_settings::keys::SEARCH_LOGGING_ENABLED,
            &serde_json::Value::Bool(val),
        )
        .await
    {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }

    if let Some(val) = body.feed_logging_enabled
        && let Err(e) = app_settings::set(
            &state.pool,
            app_settings::keys::FEED_LOGGING_ENABLED,
            &serde_json::Value::Bool(val),
        )
        .await
    {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }

    if let Some(val) = body.debrid_logging_enabled
        && let Err(e) = app_settings::set(
            &state.pool,
            app_settings::keys::DEBRID_LOGGING_ENABLED,
            &serde_json::Value::Bool(val),
        )
        .await
    {
        return (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response();
    }

    StatusCode::NO_CONTENT.into_response()
}
