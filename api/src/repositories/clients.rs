//! Download clients repository.

use super::Result;
use crate::types::DownloadClientConfig;
use sqlx::{Executor, Postgres};
use uuid::Uuid;

// =============================================================================
// Input Types
// =============================================================================

/// Input for creating a download client.
#[derive(Debug, Default)]
pub struct DownloadClientInput {
    pub name: String,
    pub client_type: String,
    pub protocol: String,
    pub base_url: String,
    pub api_key: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub category: Option<String>,
    pub download_dir: Option<String>,
    pub is_default: bool,
    pub delete_after_import: bool,
    pub seed_ratio: Option<f64>,
    pub seed_time_minutes: Option<i32>,
    pub enabled: bool,
}

/// Input for updating a download client.
#[derive(Debug, Default)]
pub struct DownloadClientUpdate {
    pub name: Option<String>,
    pub base_url: Option<String>,
    pub api_key: Option<String>,
    pub username: Option<String>,
    pub password: Option<String>,
    pub category: Option<String>,
    pub download_dir: Option<String>,
    pub is_default: Option<bool>,
    pub delete_after_import: Option<bool>,
    pub seed_ratio: Option<f64>,
    pub seed_time_minutes: Option<i32>,
    pub enabled: Option<bool>,
}

// =============================================================================
// CRUD Operations
// =============================================================================

/// List all download clients.
pub async fn list_clients<'e, E>(exec: E) -> Result<Vec<DownloadClientConfig>>
where
    E: Executor<'e, Database = Postgres>,
{
    let clients = sqlx::query_as!(
        DownloadClientConfig,
        r#"
        SELECT id, name, client_type, protocol, base_url, api_key, username, password,
               category, download_dir, is_default, delete_after_import, seed_ratio,
               seed_time_minutes, enabled, created_at, updated_at
        FROM download_clients
        ORDER BY created_at DESC
        "#
    )
    .fetch_all(exec)
    .await?;
    Ok(clients)
}

/// Get a download client by ID.
pub async fn get_client_by_id<'e, E>(exec: E, id: Uuid) -> Result<Option<DownloadClientConfig>>
where
    E: Executor<'e, Database = Postgres>,
{
    let client = sqlx::query_as!(
        DownloadClientConfig,
        r#"
        SELECT id, name, client_type, protocol, base_url, api_key, username, password,
               category, download_dir, is_default, delete_after_import, seed_ratio,
               seed_time_minutes, enabled, created_at, updated_at
        FROM download_clients
        WHERE id = $1
        "#,
        id
    )
    .fetch_optional(exec)
    .await?;
    Ok(client)
}

/// Get a download client by ID (enabled only).
pub async fn get_enabled_client_by_id<'e, E>(
    exec: E,
    id: Uuid,
) -> Result<Option<DownloadClientConfig>>
where
    E: Executor<'e, Database = Postgres>,
{
    let client = sqlx::query_as!(
        DownloadClientConfig,
        r#"
        SELECT id, name, client_type, protocol, base_url, api_key, username, password,
               category, download_dir, is_default, delete_after_import, seed_ratio,
               seed_time_minutes, enabled, created_at, updated_at
        FROM download_clients
        WHERE id = $1 AND enabled = TRUE
        "#,
        id
    )
    .fetch_optional(exec)
    .await?;
    Ok(client)
}

/// Get the default download client for a protocol.
pub async fn get_default_client_for_protocol<'e, E>(
    exec: E,
    protocol: &str,
) -> Result<Option<DownloadClientConfig>>
where
    E: Executor<'e, Database = Postgres>,
{
    let client = sqlx::query_as!(
        DownloadClientConfig,
        r#"
        SELECT id, name, client_type, protocol, base_url, api_key, username, password,
               category, download_dir, is_default, delete_after_import, seed_ratio,
               seed_time_minutes, enabled, created_at, updated_at
        FROM download_clients
        WHERE protocol = $1 AND enabled = TRUE
        ORDER BY is_default DESC, created_at ASC
        LIMIT 1
        "#,
        protocol
    )
    .fetch_optional(exec)
    .await?;
    Ok(client)
}

/// Get the default debrid download client (first enabled client with client_type='debrid').
pub async fn get_default_debrid_client<'e, E>(exec: E) -> Result<Option<DownloadClientConfig>>
where
    E: Executor<'e, Database = Postgres>,
{
    let client = sqlx::query_as!(
        DownloadClientConfig,
        r#"
        SELECT id, name, client_type, protocol, base_url, api_key, username, password,
               category, download_dir, is_default, delete_after_import, seed_ratio,
               seed_time_minutes, enabled, created_at, updated_at
        FROM download_clients
        WHERE client_type = 'debrid' AND enabled = TRUE
        ORDER BY is_default DESC, created_at ASC
        LIMIT 1
        "#,
    )
    .fetch_optional(exec)
    .await?;
    Ok(client)
}

/// Get the protocol for a download client.
pub async fn get_client_protocol<'e, E>(exec: E, id: Uuid) -> Result<Option<String>>
where
    E: Executor<'e, Database = Postgres>,
{
    let protocol = sqlx::query_scalar!("SELECT protocol FROM download_clients WHERE id = $1", id)
        .fetch_optional(exec)
        .await?;
    Ok(protocol)
}

/// Create a new download client.
pub async fn create_client<'e, E>(exec: E, input: DownloadClientInput) -> Result<Uuid>
where
    E: Executor<'e, Database = Postgres>,
{
    let id = Uuid::now_v7();
    sqlx::query!(
        r#"
        INSERT INTO download_clients (
            id, name, client_type, protocol, base_url, api_key, username, password, category,
            download_dir, is_default, delete_after_import, seed_ratio, seed_time_minutes, enabled,
            created_at, updated_at
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, NOW(), NOW())
        "#,
        id,
        &input.name,
        &input.client_type,
        &input.protocol,
        &input.base_url,
        input.api_key as Option<String>,
        input.username as Option<String>,
        input.password as Option<String>,
        input.category as Option<String>,
        input.download_dir as Option<String>,
        input.is_default,
        input.delete_after_import,
        input.seed_ratio as Option<f64>,
        input.seed_time_minutes as Option<i32>,
        input.enabled,
    )
    .execute(exec)
    .await?;
    Ok(id)
}

/// Update a download client.
pub async fn update_client<'e, E>(exec: E, id: Uuid, input: DownloadClientUpdate) -> Result<bool>
where
    E: Executor<'e, Database = Postgres>,
{
    let result = sqlx::query!(
        r#"
        UPDATE download_clients
        SET name = COALESCE($1, name),
            base_url = COALESCE($2, base_url),
            api_key = COALESCE($3, api_key),
            username = COALESCE($4, username),
            password = COALESCE($5, password),
            category = COALESCE($6, category),
            download_dir = COALESCE($7, download_dir),
            is_default = COALESCE($8, is_default),
            delete_after_import = COALESCE($9, delete_after_import),
            seed_ratio = COALESCE($10, seed_ratio),
            seed_time_minutes = COALESCE($11, seed_time_minutes),
            enabled = COALESCE($12, enabled),
            updated_at = NOW()
        WHERE id = $13
        "#,
        input.name as Option<String>,
        input.base_url as Option<String>,
        input.api_key as Option<String>,
        input.username as Option<String>,
        input.password as Option<String>,
        input.category as Option<String>,
        input.download_dir as Option<String>,
        input.is_default as Option<bool>,
        input.delete_after_import as Option<bool>,
        input.seed_ratio as Option<f64>,
        input.seed_time_minutes as Option<i32>,
        input.enabled as Option<bool>,
        id,
    )
    .execute(exec)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Clear is_default flag for all clients of a protocol except one.
pub async fn clear_default_for_protocol_except<'e, E>(
    exec: E,
    protocol: &str,
    except_id: Uuid,
) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query!(
        "UPDATE download_clients SET is_default = FALSE WHERE protocol = $1 AND id != $2",
        protocol,
        except_id
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Delete a download client.
pub async fn delete_client<'e, E>(exec: E, id: Uuid) -> Result<bool>
where
    E: Executor<'e, Database = Postgres>,
{
    let result = sqlx::query!("DELETE FROM download_clients WHERE id = $1", id)
        .execute(exec)
        .await?;
    Ok(result.rows_affected() > 0)
}
