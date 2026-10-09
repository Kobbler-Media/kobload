//! Authentication repository: users, auth tokens, API keys.

use super::Result;
use sqlx::{Executor, Postgres};
use time::OffsetDateTime;
use uuid::Uuid;

// =============================================================================
// Users
// =============================================================================

/// User summary for listing.
#[derive(Debug)]
pub struct UserRow {
    pub id: Uuid,
    pub email: String,
    pub created_at: OffsetDateTime,
}

/// User credentials for login validation.
#[derive(Debug)]
pub struct UserCredentials {
    pub id: Uuid,
    pub password_hash: String,
}

/// Get user credentials by email for login validation.
pub async fn get_user_by_email<'e, E>(exec: E, email: &str) -> Result<Option<UserCredentials>>
where
    E: Executor<'e, Database = Postgres>,
{
    let row = sqlx::query_as!(
        UserCredentials,
        "SELECT id, password_hash FROM users WHERE email = $1",
        email
    )
    .fetch_optional(exec)
    .await?;

    Ok(row)
}

/// Check if a user exists by email.
pub async fn user_exists_by_email<'e, E>(exec: E, email: &str) -> Result<bool>
where
    E: Executor<'e, Database = Postgres>,
{
    let exists: bool =
        sqlx::query_scalar!("SELECT EXISTS(SELECT 1 FROM users WHERE email = $1)", email)
            .fetch_one(exec)
            .await?
            .unwrap_or(false);
    Ok(exists)
}

/// Check if a user exists by email excluding a specific user ID.
pub async fn user_exists_by_email_excluding<'e, E>(
    exec: E,
    email: &str,
    exclude_id: Uuid,
) -> Result<bool>
where
    E: Executor<'e, Database = Postgres>,
{
    let exists: bool = sqlx::query_scalar!(
        "SELECT EXISTS(SELECT 1 FROM users WHERE email = $1 AND id != $2)",
        email,
        exclude_id
    )
    .fetch_one(exec)
    .await?
    .unwrap_or(false);
    Ok(exists)
}

/// Get a user's email by ID.
pub async fn get_user_email<'e, E>(exec: E, user_id: Uuid) -> Result<Option<String>>
where
    E: Executor<'e, Database = Postgres>,
{
    let email = sqlx::query_scalar!("SELECT email FROM users WHERE id = $1", user_id)
        .fetch_optional(exec)
        .await?;

    Ok(email)
}

/// List all users.
pub async fn list_users<'e, E>(exec: E) -> Result<Vec<UserRow>>
where
    E: Executor<'e, Database = Postgres>,
{
    let rows = sqlx::query_as!(
        UserRow,
        "SELECT id, email, created_at FROM users ORDER BY created_at DESC"
    )
    .fetch_all(exec)
    .await?;
    Ok(rows)
}

/// Get the total count of users.
pub async fn user_count<'e, E>(exec: E) -> Result<i64>
where
    E: Executor<'e, Database = Postgres>,
{
    let count: i64 = sqlx::query_scalar!("SELECT COUNT(*) FROM users")
        .fetch_one(exec)
        .await?
        .unwrap_or(0);
    Ok(count)
}

/// Create a new user.
pub async fn create_user<'e, E>(exec: E, email: &str, password_hash: &str) -> Result<Uuid>
where
    E: Executor<'e, Database = Postgres>,
{
    let id = Uuid::now_v7();
    sqlx::query!(
        "INSERT INTO users (id, email, password_hash) VALUES ($1, $2, $3)",
        id,
        email,
        password_hash
    )
    .execute(exec)
    .await?;
    Ok(id)
}

/// Update a user's email.
pub async fn update_user_email<'e, E>(exec: E, user_id: Uuid, email: &str) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query!(
        "UPDATE users SET email = $1, updated_at = NOW() WHERE id = $2",
        email,
        user_id
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Update a user's password.
pub async fn update_user_password<'e, E>(exec: E, user_id: Uuid, password_hash: &str) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query!(
        "UPDATE users SET password_hash = $1, updated_at = NOW() WHERE id = $2",
        password_hash,
        user_id
    )
    .execute(exec)
    .await?;
    Ok(())
}

// =============================================================================
// Auth Tokens (Session Tokens)
// =============================================================================

/// Create an auth token for a user session.
/// The token is SHA256-hashed before storage; only the hash is persisted.
pub async fn create_auth_token<'e, E>(exec: E, user_id: Uuid, token: &str) -> Result<Uuid>
where
    E: Executor<'e, Database = Postgres>,
{
    let id = Uuid::now_v7();
    let token_hash = crate::auth::hash_api_key(token);
    sqlx::query!(
        "INSERT INTO auth_tokens (id, user_id, token_hash, expires_at) VALUES ($1, $2, $3, NOW() + INTERVAL '30 days')",
        id,
        user_id,
        token_hash
    )
    .execute(exec)
    .await?;
    Ok(id)
}

/// Check if an auth token exists, is not revoked, and has not expired.
pub async fn auth_token_exists<'e, E>(exec: E, token: &str) -> Result<bool>
where
    E: Executor<'e, Database = Postgres>,
{
    let token_hash = crate::auth::hash_api_key(token);
    let exists: bool = sqlx::query_scalar!(
        "SELECT EXISTS(SELECT 1 FROM auth_tokens WHERE token_hash = $1 AND revoked = FALSE AND expires_at > NOW())",
        token_hash
    )
    .fetch_one(exec)
    .await?
    .unwrap_or(false);
    Ok(exists)
}

// =============================================================================
// API Keys
// =============================================================================

/// API key summary for listing.
#[derive(Debug)]
pub struct ApiKeyRow {
    pub id: Uuid,
    pub name: String,
    pub key: Option<String>,
    pub last_used_at: Option<OffsetDateTime>,
    pub created_at: OffsetDateTime,
    pub revoked: bool,
}

/// Check if an API key exists by hash.
pub async fn api_key_exists_by_hash<'e, E>(exec: E, key_hash: &str) -> Result<bool>
where
    E: Executor<'e, Database = Postgres>,
{
    let exists: bool = sqlx::query_scalar!(
        "SELECT EXISTS(SELECT 1 FROM api_keys WHERE key_hash = $1 AND revoked_at IS NULL)",
        key_hash
    )
    .fetch_one(exec)
    .await?
    .unwrap_or(false);
    Ok(exists)
}

/// Get API key ID by hash.
pub async fn get_api_key_id_by_hash<'e, E>(exec: E, key_hash: &str) -> Result<Option<Uuid>>
where
    E: Executor<'e, Database = Postgres>,
{
    let id: Option<Uuid> = sqlx::query_scalar!(
        "SELECT id FROM api_keys WHERE key_hash = $1 AND revoked_at IS NULL",
        key_hash
    )
    .fetch_optional(exec)
    .await?;
    Ok(id)
}

/// Update API key last_used_at timestamp.
pub async fn touch_api_key<'e, E>(exec: E, key_hash: &str) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query!(
        "UPDATE api_keys SET last_used_at = NOW() WHERE key_hash = $1",
        key_hash
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// List all API keys.
pub async fn list_api_keys<'e, E>(exec: E) -> Result<Vec<ApiKeyRow>>
where
    E: Executor<'e, Database = Postgres>,
{
    let rows = sqlx::query_as!(
        ApiKeyRow,
        r#"
        SELECT id, name, key, last_used_at, created_at,
               (revoked_at IS NOT NULL) as "revoked!"
        FROM api_keys
        ORDER BY created_at DESC
        "#
    )
    .fetch_all(exec)
    .await?;

    Ok(rows)
}

/// Create a new API key.
pub async fn create_api_key<'e, E>(exec: E, name: &str, key_hash: &str, key: &str) -> Result<Uuid>
where
    E: Executor<'e, Database = Postgres>,
{
    let id = Uuid::now_v7();
    sqlx::query!(
        "INSERT INTO api_keys (id, name, key_hash, key, created_at) VALUES ($1, $2, $3, $4, NOW())",
        id,
        name,
        key_hash,
        key
    )
    .execute(exec)
    .await?;
    Ok(id)
}

/// Revoke an API key.
pub async fn revoke_api_key<'e, E>(exec: E, id: Uuid) -> Result<bool>
where
    E: Executor<'e, Database = Postgres>,
{
    let result = sqlx::query!(
        "UPDATE api_keys SET revoked_at = NOW() WHERE id = $1 AND revoked_at IS NULL",
        id
    )
    .execute(exec)
    .await?;
    Ok(result.rows_affected() > 0)
}
