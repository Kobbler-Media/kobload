use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::get,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::AppState;
use crate::auth::require_auth;
use crate::repositories::{handler_type_defaults, media_types as media_types_repo};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/media-types",
            get(list_media_types).post(create_media_type),
        )
        .route(
            "/api/v1/media-types/{id}",
            get(get_media_type).patch(update_media_type),
        )
        .route(
            "/api/v1/media-types/{id}/defaults",
            get(list_defaults).post(upsert_default),
        )
}

#[derive(Debug, Serialize)]
struct MediaTypeResponse {
    id: Uuid,
    key: String,
    name: String,
    protocol_priority: Vec<String>,
    protocol_delay_minutes: Option<i32>,
    #[serde(with = "time::serde::rfc3339")]
    created_at: time::OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    updated_at: time::OffsetDateTime,
}

impl From<media_types_repo::MediaTypeConfigRow> for MediaTypeResponse {
    fn from(row: media_types_repo::MediaTypeConfigRow) -> Self {
        Self {
            id: row.id,
            key: row.key,
            name: row.name,
            protocol_priority: row.protocol_priority,
            protocol_delay_minutes: row.protocol_delay_minutes,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

async fn list_media_types(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match media_types_repo::list_all(&state.pool).await {
        Ok(rows) => {
            let items: Vec<MediaTypeResponse> = rows.into_iter().map(Into::into).collect();
            Json(items).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

async fn get_media_type(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match media_types_repo::get_by_id(&state.pool, id).await {
        Ok(Some(row)) => Json(MediaTypeResponse::from(row)).into_response(),
        Ok(None) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Debug, Deserialize)]
struct CreateMediaType {
    key: String,
    name: String,
}

async fn create_media_type(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<CreateMediaType>,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match media_types_repo::upsert(&state.pool, &body.key, &body.name).await {
        Ok(id) => (StatusCode::CREATED, Json(serde_json::json!({ "id": id }))).into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Debug, Deserialize)]
struct UpdateMediaType {
    name: Option<String>,
    protocol_priority: Option<Vec<String>>,
    protocol_delay_minutes: Option<Option<i32>>,
}

async fn update_media_type(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<UpdateMediaType>,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let input = media_types_repo::MediaTypeConfigUpdate {
        name: body.name,
        protocol_priority: body.protocol_priority,
        protocol_delay_minutes: body.protocol_delay_minutes,
    };
    match media_types_repo::update(&state.pool, id, input).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => StatusCode::NOT_FOUND.into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Debug, Serialize)]
struct HandlerTypeDefaultResponse {
    id: Uuid,
    handler_type: String,
    media_type_id: Uuid,
    category_ids: Vec<String>,
}

async fn list_defaults(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(_id): Path<Uuid>,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match handler_type_defaults::list_all(&state.pool).await {
        Ok(rows) => {
            let items: Vec<HandlerTypeDefaultResponse> = rows
                .into_iter()
                .map(|r| HandlerTypeDefaultResponse {
                    id: r.id,
                    handler_type: r.handler_type,
                    media_type_id: r.media_type_id,
                    category_ids: r.category_ids,
                })
                .collect();
            Json(items).into_response()
        }
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}

#[derive(Debug, Deserialize)]
struct UpsertDefault {
    handler_type: String,
    category_ids: Vec<String>,
}

async fn upsert_default(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<UpsertDefault>,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match handler_type_defaults::upsert(&state.pool, &body.handler_type, id, &body.category_ids)
        .await
    {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(e) => (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response(),
    }
}
