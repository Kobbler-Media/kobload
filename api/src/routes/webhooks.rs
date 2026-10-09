use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    routing::{get, put},
};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::auth::require_auth;
use crate::repositories::webhooks;
use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/admin/webhooks",
            get(list_webhooks).post(create_webhook),
        )
        .route(
            "/api/v1/admin/webhooks/{id}",
            put(update_webhook).delete(delete_webhook),
        )
}

#[derive(Debug, Deserialize)]
struct CreateWebhookRequest {
    url: String,
    secret: Option<String>,
    /// Event types to subscribe to. Empty = all events.
    /// Valid events: "download.created", "download.started", "download.completed",
    /// "download.failed", "download.cancelled"
    #[serde(default)]
    events: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct UpdateWebhookRequest {
    url: Option<String>,
    secret: Option<Option<String>>,
    events: Option<Vec<String>>,
    enabled: Option<bool>,
}

#[derive(Debug, Serialize)]
struct WebhookResponse {
    id: Uuid,
    url: String,
    events: Vec<String>,
    enabled: bool,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    updated_at: OffsetDateTime,
}

impl From<webhooks::StatusWebhook> for WebhookResponse {
    fn from(w: webhooks::StatusWebhook) -> Self {
        Self {
            id: w.id,
            url: w.url,
            events: w.events,
            enabled: w.enabled,
            created_at: w.created_at,
            updated_at: w.updated_at,
        }
    }
}

async fn list_webhooks(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<WebhookResponse>>, (StatusCode, String)> {
    if require_auth(&state, &headers).await.is_err() {
        return Err((StatusCode::UNAUTHORIZED, "Unauthorized".to_string()));
    }

    let hooks = webhooks::list(&state.pool)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(hooks.into_iter().map(WebhookResponse::from).collect()))
}

async fn create_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(req): Json<CreateWebhookRequest>,
) -> Result<(StatusCode, Json<WebhookResponse>), (StatusCode, String)> {
    if require_auth(&state, &headers).await.is_err() {
        return Err((StatusCode::UNAUTHORIZED, "Unauthorized".to_string()));
    }

    let id = webhooks::create(
        &state.pool,
        webhooks::WebhookInput {
            url: req.url.clone(),
            secret: req.secret,
            events: req.events.clone(),
        },
    )
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let now = OffsetDateTime::now_utc();
    Ok((
        StatusCode::CREATED,
        Json(WebhookResponse {
            id,
            url: req.url,
            events: req.events,
            enabled: true,
            created_at: now,
            updated_at: now,
        }),
    ))
}

async fn update_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(req): Json<UpdateWebhookRequest>,
) -> Result<StatusCode, (StatusCode, String)> {
    if require_auth(&state, &headers).await.is_err() {
        return Err((StatusCode::UNAUTHORIZED, "Unauthorized".to_string()));
    }

    let found = webhooks::update(
        &state.pool,
        id,
        webhooks::WebhookUpdate {
            url: req.url,
            secret: req.secret,
            events: req.events,
            enabled: req.enabled,
        },
    )
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    if found {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err((StatusCode::NOT_FOUND, "Webhook not found".to_string()))
    }
}

async fn delete_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<StatusCode, (StatusCode, String)> {
    if require_auth(&state, &headers).await.is_err() {
        return Err((StatusCode::UNAUTHORIZED, "Unauthorized".to_string()));
    }

    webhooks::delete(&state.pool, id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(StatusCode::NO_CONTENT)
}
