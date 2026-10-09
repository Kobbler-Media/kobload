use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use base64::{Engine as _, engine::general_purpose};
use rand::RngExt;
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use tracing::warn;
use uuid::Uuid;

use crate::auth::{hash_api_key, hash_password, require_auth, verify_password};
use crate::helpers::{normalize_email, validate_password};
use crate::repositories::auth as auth_repo;
use crate::state::AppState;

#[derive(Debug, Deserialize)]
pub struct LoginRequest {
    email: String,
    password: String,
}

#[derive(Debug, Serialize)]
pub struct LoginResponse {
    token: String,
}

#[derive(Debug, Serialize, sqlx::FromRow)]
pub struct UserSummary {
    id: Uuid,
    email: String,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
}

#[derive(Debug, Deserialize)]
pub struct CreateUserRequest {
    email: String,
    password: String,
}

#[derive(Debug, Serialize)]
pub struct CreateUserResponse {
    user_id: Uuid,
}

#[derive(Debug, Deserialize)]
pub struct UpdateUserRequest {
    email: Option<String>,
    password: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct UpdateUserResponse {
    user_id: Uuid,
    email: String,
}

#[derive(Debug, Serialize)]
pub struct ApiKeySummary {
    id: Uuid,
    name: String,
    key: Option<String>,
    #[serde(with = "time::serde::rfc3339::option")]
    last_used_at: Option<OffsetDateTime>,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
    revoked: bool,
}

#[derive(Debug, Deserialize)]
pub struct CreateApiKeyRequest {
    name: String,
}

#[derive(Debug, Serialize)]
pub struct CreateApiKeyResponse {
    id: Uuid,
    key: String,
}

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/admin/login", post(login))
        .route("/api/v1/admin/users", get(list_users).post(create_user))
        .route(
            "/api/v1/admin/users/{id}",
            axum::routing::patch(update_user),
        )
        .route(
            "/api/v1/admin/api-keys",
            get(list_api_keys).post(create_api_key),
        )
        .route(
            "/api/v1/admin/api-keys/{id}",
            axum::routing::delete(revoke_api_key),
        )
}

async fn login(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<LoginRequest>,
) -> impl IntoResponse {
    let ip = headers
        .get("x-forwarded-for")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(',').next())
        .and_then(|s| s.trim().parse::<std::net::IpAddr>().ok())
        .unwrap_or(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED));

    if !state.login_limiter.check(ip) {
        return StatusCode::TOO_MANY_REQUESTS.into_response();
    }

    let credentials = match auth_repo::get_user_by_email(&state.pool, &payload.email).await {
        Ok(Some(creds)) => creds,
        Ok(None) => return (StatusCode::UNAUTHORIZED, "invalid credentials").into_response(),
        Err(err) => {
            warn!(error = %err, "Failed to fetch user");
            return (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response();
        }
    };

    if !verify_password(&payload.password, &credentials.password_hash) {
        return (StatusCode::UNAUTHORIZED, "invalid credentials").into_response();
    }

    let mut raw = [0u8; 32];
    rand::rng().fill(&mut raw[..]);
    let token = general_purpose::URL_SAFE_NO_PAD.encode(raw);

    let result = auth_repo::create_auth_token(&state.pool, credentials.id, &token).await;

    if let Err(err) = result {
        warn!(error = %err, "Failed to create auth token");
        return (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response();
    }

    Json(LoginResponse { token }).into_response()
}

async fn list_users(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    match auth_repo::list_users(&state.pool).await {
        Ok(users) => {
            let summaries: Vec<UserSummary> = users
                .into_iter()
                .map(|u| UserSummary {
                    id: u.id,
                    email: u.email,
                    created_at: u.created_at,
                })
                .collect();
            Json(summaries).into_response()
        }
        Err(err) => {
            warn!(error = %err, "Failed to list users");
            (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response()
        }
    }
}

async fn create_user(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<CreateUserRequest>,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let email = match normalize_email(&payload.email) {
        Ok(email) => email,
        Err(_) => return (StatusCode::BAD_REQUEST, "email and password required").into_response(),
    };
    if validate_password(&payload.password, false).is_err() {
        return (StatusCode::BAD_REQUEST, "email and password required").into_response();
    }

    match auth_repo::user_exists_by_email(&state.pool, &email).await {
        Ok(true) => return (StatusCode::BAD_REQUEST, "email already exists").into_response(),
        Err(err) => {
            warn!(error = %err, "Failed to check user existence");
            return (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response();
        }
        Ok(false) => {}
    }

    let password_hash = match hash_password(&payload.password) {
        Ok(hash) => hash,
        Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, "hash failed").into_response(),
    };

    match auth_repo::create_user(&state.pool, &email, &password_hash).await {
        Ok(user_id) => Json(CreateUserResponse { user_id }).into_response(),
        Err(err) => {
            warn!(error = %err, "Failed to create user");
            (StatusCode::BAD_REQUEST, "failed to create user").into_response()
        }
    }
}

async fn update_user(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(user_id): Path<Uuid>,
    Json(payload): Json<UpdateUserRequest>,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let mut email = None;
    if let Some(val) = payload.email {
        match normalize_email(&val) {
            Ok(trimmed) => email = Some(trimmed),
            Err(_) => return (StatusCode::BAD_REQUEST, "email cannot be empty").into_response(),
        }
    }

    if let Some(ref new_email) = email {
        match auth_repo::user_exists_by_email_excluding(&state.pool, new_email, user_id).await {
            Ok(true) => return (StatusCode::BAD_REQUEST, "email already exists").into_response(),
            Err(err) => {
                warn!(error = %err, "Failed to check email existence");
                return (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response();
            }
            Ok(false) => {}
        }
    }

    let existing_email = match auth_repo::get_user_email(&state.pool, user_id).await {
        Ok(Some(email)) => email,
        Ok(None) => return (StatusCode::NOT_FOUND, "user not found").into_response(),
        Err(err) => {
            warn!(error = %err, "Failed to load user");
            return (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response();
        }
    };

    if let Some(password) = payload.password {
        if validate_password(&password, false).is_err() {
            return (StatusCode::BAD_REQUEST, "password cannot be empty").into_response();
        }
        let password_hash = match hash_password(&password) {
            Ok(hash) => hash,
            Err(_) => return (StatusCode::INTERNAL_SERVER_ERROR, "hash failed").into_response(),
        };
        if auth_repo::update_user_password(&state.pool, user_id, &password_hash)
            .await
            .is_err()
        {
            return (StatusCode::NOT_FOUND, "user not found").into_response();
        }
    }

    let updated_email = if let Some(email) = email.clone() {
        match auth_repo::update_user_email(&state.pool, user_id, &email).await {
            Ok(_) => email,
            Err(_) => return (StatusCode::NOT_FOUND, "user not found").into_response(),
        }
    } else {
        existing_email
    };

    Json(UpdateUserResponse {
        user_id,
        email: updated_email,
    })
    .into_response()
}

async fn list_api_keys(State(state): State<AppState>, headers: HeaderMap) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    match auth_repo::list_api_keys(&state.pool).await {
        Ok(keys) => {
            let summaries: Vec<ApiKeySummary> = keys
                .into_iter()
                .map(|k| ApiKeySummary {
                    id: k.id,
                    name: k.name,
                    key: k.key,
                    last_used_at: k.last_used_at,
                    created_at: k.created_at,
                    revoked: k.revoked,
                })
                .collect();
            Json(summaries).into_response()
        }
        Err(err) => {
            warn!(error = %err, "Failed to list API keys");
            (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response()
        }
    }
}

async fn create_api_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<CreateApiKeyRequest>,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let name = payload.name.trim();
    if name.is_empty() {
        return (StatusCode::BAD_REQUEST, "name is required").into_response();
    }

    // Generate a secure random key
    let mut key_bytes = [0u8; 32];
    rand::rng().fill(&mut key_bytes[..]);
    let key = general_purpose::URL_SAFE_NO_PAD.encode(key_bytes);
    let key_hash = hash_api_key(&key);

    match auth_repo::create_api_key(&state.pool, name, &key_hash, &key).await {
        Ok(id) => Json(CreateApiKeyResponse { id, key }).into_response(),
        Err(err) => {
            warn!(error = %err, "Failed to create API key");
            (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response()
        }
    }
}

async fn revoke_api_key(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    match auth_repo::revoke_api_key(&state.pool, id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => (StatusCode::NOT_FOUND, "not found or already revoked").into_response(),
        Err(err) => {
            warn!(error = %err, "Failed to revoke API key");
            (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response()
        }
    }
}
