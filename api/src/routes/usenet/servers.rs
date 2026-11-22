use axum::Json;
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use tracing::warn;
use uuid::Uuid;

use crate::auth::require_auth;
use crate::repositories::usenet_servers::{self as repo, UsenetServerInput, UsenetServerRow};
use crate::state::AppState;
use crate::usenet::config::{format_date, parse_date};
use crate::usenet::{ProbeTarget, probe, sab_servers, same_host, violation};

#[derive(Debug, Serialize)]
pub struct UsenetServerView {
    id: Uuid,
    name: String,
    host: String,
    port: i32,
    tls: bool,
    verify_cert: bool,
    ip_preference: String,
    connections: i32,
    priority: i32,
    backbone: Option<String>,
    retention_days: Option<i32>,
    expires_on: Option<String>,
    quota_bytes: Option<i64>,
    username: String,
    has_password: bool,
    account_connection_cap: Option<i32>,
    enabled: bool,
    #[serde(with = "time::serde::rfc3339")]
    created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    updated_at: OffsetDateTime,
}

impl From<UsenetServerRow> for UsenetServerView {
    fn from(row: UsenetServerRow) -> Self {
        Self {
            id: row.id,
            name: row.name,
            host: row.host,
            port: row.port,
            tls: row.tls,
            verify_cert: row.verify_cert,
            ip_preference: row.ip_preference,
            connections: row.connections,
            priority: row.priority,
            backbone: row.backbone,
            retention_days: row.retention_days,
            expires_on: row.expires_on.map(format_date),
            quota_bytes: row.quota_bytes,
            username: row.username,
            has_password: !row.password.is_empty(),
            account_connection_cap: row.account_connection_cap,
            enabled: row.enabled,
            created_at: row.created_at,
            updated_at: row.updated_at,
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_ip_preference() -> String {
    "any".to_string()
}

#[derive(Deserialize)]
pub struct UsenetServerBody {
    name: String,
    host: String,
    port: i32,
    #[serde(default = "default_true")]
    tls: bool,
    #[serde(default = "default_true")]
    verify_cert: bool,
    #[serde(default = "default_ip_preference")]
    ip_preference: String,
    connections: i32,
    #[serde(default)]
    priority: i32,
    #[serde(default)]
    backbone: Option<String>,
    #[serde(default)]
    retention_days: Option<i32>,
    #[serde(default)]
    expires_on: Option<String>,
    #[serde(default)]
    quota_bytes: Option<i64>,
    #[serde(default)]
    username: String,
    #[serde(default)]
    password: Option<String>,
    #[serde(default)]
    account_connection_cap: Option<i32>,
    #[serde(default = "default_true")]
    enabled: bool,
}

#[derive(Deserialize)]
pub struct TestBody {
    #[serde(default)]
    id: Option<Uuid>,
    #[serde(flatten)]
    server: UsenetServerBody,
}

#[derive(Debug, Serialize)]
pub struct TestResult {
    success: bool,
    message: String,
}

fn blank(value: Option<String>) -> Option<String> {
    value
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

const PASSWORD_AGAIN: &str = "Enter the password again: it is only sent to the host, port, TLS and certificate check it was saved for";

impl UsenetServerBody {
    // NNTP is line-based, so a line break in a credential would smuggle a command.
    fn check_credentials(&self) -> Result<(), String> {
        let broken = |v: &str| v.contains(['\r', '\n']);
        if broken(&self.username) || self.password.as_deref().is_some_and(broken) {
            return Err("The username and password cannot contain line breaks".to_string());
        }
        Ok(())
    }

    fn validate(self) -> Result<UsenetServerInput, String> {
        self.check_credentials()?;
        if self.name.trim().is_empty() || self.host.trim().is_empty() {
            return Err("A server needs a name and a host".to_string());
        }
        if !(1..=65535).contains(&self.port) {
            return Err(format!("Port {} is out of range", self.port));
        }
        if self.connections < 1 {
            return Err("A server needs at least one connection".to_string());
        }
        if self.priority < 0 {
            return Err("The priority tier cannot be negative".to_string());
        }
        if !["any", "v4", "v6"].contains(&self.ip_preference.as_str()) {
            return Err(format!("Unknown IP preference {}", self.ip_preference));
        }
        let positive = |v: Option<i64>, what: &str| match v {
            Some(n) if n < 1 => Err(format!("{what} must be positive")),
            _ => Ok(()),
        };
        positive(self.retention_days.map(i64::from), "Retention")?;
        positive(self.quota_bytes, "The quota")?;
        positive(
            self.account_connection_cap.map(i64::from),
            "The connection cap",
        )?;
        let expires_on = match blank(self.expires_on) {
            Some(date) => {
                Some(parse_date(&date).ok_or_else(|| format!("{date} is not a YYYY-MM-DD date"))?)
            }
            None => None,
        };
        Ok(UsenetServerInput {
            name: self.name.trim().to_string(),
            host: self.host.trim().to_string(),
            port: self.port,
            tls: self.tls,
            verify_cert: self.verify_cert,
            ip_preference: self.ip_preference,
            connections: self.connections,
            priority: self.priority,
            backbone: blank(self.backbone),
            retention_days: self.retention_days,
            expires_on,
            quota_bytes: self.quota_bytes,
            username: self.username.trim().to_string(),
            password: blank_password(self.password),
            account_connection_cap: self.account_connection_cap,
            enabled: self.enabled,
        })
    }
}

fn candidate(id: Uuid, input: &UsenetServerInput) -> UsenetServerRow {
    let now = OffsetDateTime::now_utc();
    UsenetServerRow {
        id,
        name: input.name.clone(),
        host: input.host.clone(),
        port: input.port,
        tls: input.tls,
        verify_cert: input.verify_cert,
        ip_preference: input.ip_preference.clone(),
        connections: input.connections,
        priority: input.priority,
        backbone: input.backbone.clone(),
        retention_days: input.retention_days,
        expires_on: input.expires_on,
        quota_bytes: input.quota_bytes,
        username: input.username.clone(),
        password: String::new(),
        account_connection_cap: input.account_connection_cap,
        enabled: input.enabled,
        created_at: now,
        updated_at: now,
    }
}

// Turning certificate checks off would let an impostor of the same host collect the
// password, so that counts as a new endpoint too.
fn same_endpoint(row: &UsenetServerRow, host: &str, port: i32, tls: bool, verify: bool) -> bool {
    same_host(&row.host, host) && row.port == port && row.tls == tls && (verify || !row.verify_cert)
}

fn database_error(e: impl std::fmt::Display) -> Response {
    warn!(error = %e, "Usenet server query failed");
    (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response()
}

/// Every server of one login shares the provider's connection cap, so a save that would
/// push the login over it is refused.
async fn checked(
    state: &AppState,
    id: Uuid,
    body: UsenetServerBody,
) -> Result<UsenetServerInput, Response> {
    let input = body
        .validate()
        .map_err(|e| (StatusCode::BAD_REQUEST, e).into_response())?;
    let stored = repo::list(&state.pool).await.map_err(database_error)?;
    if input.password.is_none()
        && let Some(row) = stored.iter().find(|r| r.id == id)
        && !row.password.is_empty()
        && !same_endpoint(row, &input.host, input.port, input.tls, input.verify_cert)
    {
        return Err((StatusCode::BAD_REQUEST, PASSWORD_AGAIN).into_response());
    }
    let sab = sab_servers(&state.pool, &state.http_client).await;
    if let Some(over) = violation(&stored, &candidate(id, &input), &sab) {
        return Err((StatusCode::BAD_REQUEST, over.describe()).into_response());
    }
    Ok(input)
}

pub async fn list(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match repo::list(&state.pool).await {
        Ok(rows) => Json(
            rows.into_iter()
                .map(UsenetServerView::from)
                .collect::<Vec<_>>(),
        )
        .into_response(),
        Err(e) => database_error(e),
    }
}

pub async fn get(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Response {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match repo::get(&state.pool, id).await {
        Ok(Some(row)) => Json(UsenetServerView::from(row)).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, "not found").into_response(),
        Err(e) => database_error(e),
    }
}

pub async fn create(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<UsenetServerBody>,
) -> Response {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let input = match checked(&state, Uuid::nil(), body).await {
        Ok(input) => input,
        Err(resp) => return resp,
    };
    match repo::create(&state.pool, &input).await {
        Ok(id) => Json(serde_json::json!({ "id": id })).into_response(),
        Err(e) => database_error(e),
    }
}

pub async fn update(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(body): Json<UsenetServerBody>,
) -> Response {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let input = match checked(&state, id, body).await {
        Ok(input) => input,
        Err(resp) => return resp,
    };
    match repo::update(&state.pool, id, &input).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => (StatusCode::NOT_FOUND, "not found").into_response(),
        Err(e) => database_error(e),
    }
}

pub async fn delete(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Response {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match repo::delete(&state.pool, id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => (StatusCode::NOT_FOUND, "not found").into_response(),
        Err(e) => database_error(e),
    }
}

async fn run_probe(
    host: &str,
    port: i32,
    tls: bool,
    verify_cert: bool,
    username: &str,
    password: &str,
) -> Json<TestResult> {
    let Ok(port) = u16::try_from(port) else {
        return Json(TestResult {
            success: false,
            message: format!("Port {port} is out of range"),
        });
    };
    let target = ProbeTarget {
        host,
        port,
        tls,
        verify_cert,
        username,
        password,
    };
    Json(match probe(&target).await {
        Ok(message) => TestResult {
            success: true,
            message,
        },
        Err(message) => TestResult {
            success: false,
            message,
        },
    })
}

pub async fn test_unsaved(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<TestBody>,
) -> Response {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let server = body.server;
    if let Err(e) = server.check_credentials() {
        return (StatusCode::BAD_REQUEST, e).into_response();
    }
    let password = match (blank_password(server.password.clone()), body.id) {
        (Some(typed), _) => typed,
        (None, None) => String::new(),
        (None, Some(id)) => match repo::get(&state.pool, id).await {
            Ok(Some(row))
                if row.password.is_empty()
                    || same_endpoint(
                        &row,
                        &server.host,
                        server.port,
                        server.tls,
                        server.verify_cert,
                    ) =>
            {
                row.password
            }
            Ok(Some(_)) => {
                return Json(TestResult {
                    success: false,
                    message: PASSWORD_AGAIN.to_string(),
                })
                .into_response();
            }
            Ok(None) => return (StatusCode::NOT_FOUND, "not found").into_response(),
            Err(e) => return database_error(e),
        },
    };
    run_probe(
        server.host.trim(),
        server.port,
        server.tls,
        server.verify_cert,
        server.username.trim(),
        &password,
    )
    .await
    .into_response()
}

fn blank_password(password: Option<String>) -> Option<String> {
    password.filter(|p| !p.is_empty())
}

pub async fn test_saved(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Response {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    match repo::get(&state.pool, id).await {
        Ok(Some(row)) => run_probe(
            &row.host,
            row.port,
            row.tls,
            row.verify_cert,
            &row.username,
            &row.password,
        )
        .await
        .into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, "not found").into_response(),
        Err(e) => database_error(e),
    }
}
