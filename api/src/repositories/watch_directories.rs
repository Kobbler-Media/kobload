//! Watch directories repository.

use super::Result;
use serde::{Deserialize, Serialize};
use sqlx::{Executor, FromRow, Postgres};
use time::OffsetDateTime;
use uuid::Uuid;

// =============================================================================
// Types
// =============================================================================

/// A watched directory configuration.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct WatchDirectory {
    pub id: Uuid,
    pub path: String,
    pub name: String,
    pub enabled: bool,
    pub recursive: bool,
    pub media_type: String,
    pub callback_url: String,
    pub auth_token: Option<String>,
    pub scan_status: String,
    pub scan_error: Option<String>,
    pub error_count: i32,
    pub next_retry_at: Option<OffsetDateTime>,
    pub last_scan_at: Option<OffsetDateTime>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

/// A file discovered in a watched directory.
#[derive(Debug, Clone, Serialize, Deserialize, FromRow)]
pub struct WatchDirectoryFile {
    pub id: Uuid,
    pub watch_directory_id: Uuid,
    pub file_path: String,
    pub file_name: String,
    pub file_size: i64,
    pub file_hash: Option<String>,
    pub file_mtime: Option<OffsetDateTime>,
    pub interpretation: Option<serde_json::Value>,
    pub status: String,
    pub download_id: Option<Uuid>,
    pub callback_sent_at: Option<OffsetDateTime>,
    pub callback_error: Option<String>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

/// Input for creating a watch directory.
#[derive(Debug, Default)]
pub struct WatchDirectoryInput {
    pub path: String,
    pub name: String,
    pub enabled: bool,
    pub recursive: bool,
    pub media_type: String,
    pub callback_url: String,
    pub auth_token: Option<String>,
}

/// Input for updating a watch directory.
#[derive(Debug, Default)]
pub struct WatchDirectoryUpdate {
    pub name: Option<String>,
    pub path: Option<String>,
    pub enabled: Option<bool>,
    pub recursive: Option<bool>,
    pub media_type: Option<String>,
    pub callback_url: Option<String>,
    pub auth_token: Option<String>,
}

/// Input for upserting a discovered file.
#[derive(Debug)]
pub struct WatchDirectoryFileInput {
    pub watch_directory_id: Uuid,
    pub file_path: String,
    pub file_name: String,
    pub file_size: i64,
    pub file_hash: Option<String>,
    pub file_mtime: Option<OffsetDateTime>,
    pub interpretation: Option<serde_json::Value>,
}

// =============================================================================
// Directory CRUD
// =============================================================================

/// Create a new watch directory.
pub async fn create<'e, E>(exec: E, input: WatchDirectoryInput) -> Result<WatchDirectory>
where
    E: Executor<'e, Database = Postgres>,
{
    let id = Uuid::now_v7();
    let dir = sqlx::query_as!(
        WatchDirectory,
        r#"
        INSERT INTO watch_directories (id, path, name, enabled, recursive, media_type, callback_url, auth_token)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
        RETURNING *
        "#,
        id,
        &input.path,
        &input.name,
        input.enabled,
        input.recursive,
        &input.media_type,
        &input.callback_url,
        input.auth_token.as_deref(),
    )
    .fetch_one(exec)
    .await?;
    Ok(dir)
}

/// Update a watch directory.
pub async fn update<'e, E>(exec: E, id: Uuid, input: WatchDirectoryUpdate) -> Result<bool>
where
    E: Executor<'e, Database = Postgres>,
{
    let result = sqlx::query!(
        r#"
        UPDATE watch_directories
        SET name = COALESCE($1, name),
            path = COALESCE($2, path),
            enabled = COALESCE($3, enabled),
            recursive = COALESCE($4, recursive),
            media_type = COALESCE($5, media_type),
            callback_url = COALESCE($6, callback_url),
            auth_token = COALESCE($7, auth_token),
            updated_at = NOW()
        WHERE id = $8
        "#,
        input.name.as_deref(),
        input.path.as_deref(),
        input.enabled,
        input.recursive,
        input.media_type.as_deref(),
        input.callback_url.as_deref(),
        input.auth_token.as_deref(),
        id,
    )
    .execute(exec)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Delete a watch directory (cascades to files).
pub async fn delete<'e, E>(exec: E, id: Uuid) -> Result<bool>
where
    E: Executor<'e, Database = Postgres>,
{
    let result = sqlx::query!("DELETE FROM watch_directories WHERE id = $1", id)
        .execute(exec)
        .await?;
    Ok(result.rows_affected() > 0)
}

/// Get a watch directory by ID.
pub async fn get_by_id<'e, E>(exec: E, id: Uuid) -> Result<Option<WatchDirectory>>
where
    E: Executor<'e, Database = Postgres>,
{
    let dir = sqlx::query_as!(
        WatchDirectory,
        "SELECT * FROM watch_directories WHERE id = $1",
        id
    )
    .fetch_optional(exec)
    .await?;
    Ok(dir)
}

/// List all watch directories.
pub async fn list_all<'e, E>(exec: E) -> Result<Vec<WatchDirectory>>
where
    E: Executor<'e, Database = Postgres>,
{
    let dirs = sqlx::query_as!(
        WatchDirectory,
        "SELECT * FROM watch_directories ORDER BY created_at DESC"
    )
    .fetch_all(exec)
    .await?;
    Ok(dirs)
}

/// List enabled watch directories.
pub async fn list_enabled<'e, E>(exec: E) -> Result<Vec<WatchDirectory>>
where
    E: Executor<'e, Database = Postgres>,
{
    let dirs = sqlx::query_as!(
        WatchDirectory,
        "SELECT * FROM watch_directories WHERE enabled = TRUE ORDER BY created_at DESC"
    )
    .fetch_all(exec)
    .await?;
    Ok(dirs)
}

// =============================================================================
// Scan State
// =============================================================================

/// Mark a directory as scanning.
pub async fn set_scanning<'e, E>(exec: E, id: Uuid) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query!(
        "UPDATE watch_directories SET scan_status = 'scanning', updated_at = NOW() WHERE id = $1",
        id
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Mark a scan as complete.
pub async fn set_scan_complete<'e, E>(exec: E, id: Uuid) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query!(
        r#"
        UPDATE watch_directories
        SET scan_status = 'idle',
            scan_error = NULL,
            error_count = 0,
            next_retry_at = NULL,
            last_scan_at = NOW(),
            updated_at = NOW()
        WHERE id = $1
        "#,
        id
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Record a scan error with exponential backoff.
pub async fn record_scan_error<'e, E>(exec: E, id: Uuid, error: &str) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    // Backoff: 1m, 2m, 4m, 8m, 16m, 32m, 60m capped
    sqlx::query!(
        r#"
        UPDATE watch_directories
        SET scan_status = 'error',
            scan_error = $2,
            error_count = error_count + 1,
            next_retry_at = NOW() + LEAST(
                INTERVAL '1 minute' * POWER(2, LEAST(error_count, 5)),
                INTERVAL '60 minutes'
            ),
            updated_at = NOW()
        WHERE id = $1
        "#,
        id,
        error,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Clear error state (for manual retry).
pub async fn clear_error<'e, E>(exec: E, id: Uuid) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query!(
        r#"
        UPDATE watch_directories
        SET scan_status = 'idle',
            scan_error = NULL,
            error_count = 0,
            next_retry_at = NULL,
            updated_at = NOW()
        WHERE id = $1
        "#,
        id
    )
    .execute(exec)
    .await?;
    Ok(())
}

// =============================================================================
// File Operations
// =============================================================================

/// Upsert a discovered file (insert or update if path already exists).
pub async fn upsert_file<'e, E>(
    exec: E,
    input: WatchDirectoryFileInput,
) -> Result<WatchDirectoryFile>
where
    E: Executor<'e, Database = Postgres>,
{
    let id = Uuid::now_v7();
    let file = sqlx::query_as!(
        WatchDirectoryFile,
        r#"
        INSERT INTO watch_directory_files
            (id, watch_directory_id, file_path, file_name, file_size, file_hash, file_mtime, interpretation)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
        ON CONFLICT (watch_directory_id, file_path) DO UPDATE SET
            file_size = EXCLUDED.file_size,
            file_hash = EXCLUDED.file_hash,
            file_mtime = EXCLUDED.file_mtime,
            interpretation = EXCLUDED.interpretation,
            status = 'discovered',
            callback_sent_at = NULL,
            callback_error = NULL,
            updated_at = NOW()
        RETURNING *
        "#,
        id,
        input.watch_directory_id,
        &input.file_path,
        &input.file_name,
        input.file_size,
        input.file_hash.as_deref(),
        input.file_mtime,
        input.interpretation,
    )
    .fetch_one(exec)
    .await?;
    Ok(file)
}

/// Mark a file as sent (callback succeeded).
pub async fn mark_file_sent<'e, E>(exec: E, file_id: Uuid) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query!(
        r#"
        UPDATE watch_directory_files
        SET status = 'sent', callback_sent_at = NOW(), callback_error = NULL, updated_at = NOW()
        WHERE id = $1
        "#,
        file_id
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Mark a file as skipped (matched a download).
pub async fn mark_file_skipped<'e, E>(
    exec: E,
    file_id: Uuid,
    download_id: Option<Uuid>,
) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query!(
        r#"
        UPDATE watch_directory_files
        SET status = 'skipped', download_id = $2, updated_at = NOW()
        WHERE id = $1
        "#,
        file_id,
        download_id,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Mark a file as error (callback failed).
pub async fn mark_file_error<'e, E>(exec: E, file_id: Uuid, error: &str) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query!(
        r#"
        UPDATE watch_directory_files
        SET status = 'error', callback_error = $2, updated_at = NOW()
        WHERE id = $1
        "#,
        file_id,
        error,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Get a file by ID.
pub async fn get_file_by_id<'e, E>(exec: E, file_id: Uuid) -> Result<Option<WatchDirectoryFile>>
where
    E: Executor<'e, Database = Postgres>,
{
    let file = sqlx::query_as!(
        WatchDirectoryFile,
        "SELECT * FROM watch_directory_files WHERE id = $1",
        file_id
    )
    .fetch_optional(exec)
    .await?;
    Ok(file)
}

/// List files for a watch directory with pagination.
pub async fn list_files<'e, E>(
    exec: E,
    dir_id: Uuid,
    limit: i64,
    offset: i64,
) -> Result<Vec<WatchDirectoryFile>>
where
    E: Executor<'e, Database = Postgres>,
{
    let files = sqlx::query_as!(
        WatchDirectoryFile,
        r#"
        SELECT * FROM watch_directory_files
        WHERE watch_directory_id = $1
        ORDER BY created_at DESC
        LIMIT $2 OFFSET $3
        "#,
        dir_id,
        limit,
        offset,
    )
    .fetch_all(exec)
    .await?;
    Ok(files)
}

/// Get files that need callbacks dispatched (status = 'discovered').
pub async fn get_undispatched_files<'e, E>(exec: E, dir_id: Uuid) -> Result<Vec<WatchDirectoryFile>>
where
    E: Executor<'e, Database = Postgres>,
{
    let files = sqlx::query_as!(
        WatchDirectoryFile,
        r#"
        SELECT * FROM watch_directory_files
        WHERE watch_directory_id = $1 AND status = 'discovered'
        ORDER BY created_at ASC
        "#,
        dir_id,
    )
    .fetch_all(exec)
    .await?;
    Ok(files)
}

/// Get file by path within a directory (for dedup during scan).
pub async fn get_file_by_path<'e, E>(
    exec: E,
    dir_id: Uuid,
    file_path: &str,
) -> Result<Option<WatchDirectoryFile>>
where
    E: Executor<'e, Database = Postgres>,
{
    let file = sqlx::query_as!(
        WatchDirectoryFile,
        r#"
        SELECT * FROM watch_directory_files
        WHERE watch_directory_id = $1 AND file_path = $2
        "#,
        dir_id,
        file_path,
    )
    .fetch_optional(exec)
    .await?;
    Ok(file)
}

/// Get file count for a directory grouped by status.
pub async fn file_status_counts<'e, E>(exec: E, dir_id: Uuid) -> Result<Vec<FileStatusCount>>
where
    E: Executor<'e, Database = Postgres>,
{
    let counts = sqlx::query_as!(
        FileStatusCount,
        r#"
        SELECT status, COUNT(*) as "count!"
        FROM watch_directory_files
        WHERE watch_directory_id = $1
        GROUP BY status
        "#,
        dir_id,
    )
    .fetch_all(exec)
    .await?;
    Ok(counts)
}

/// Status count row.
#[derive(Debug, Serialize)]
pub struct FileStatusCount {
    pub status: String,
    pub count: i64,
}

// =============================================================================
// Dedup
// =============================================================================

/// Check if a file path matches any non-failed download.
pub async fn file_exists_in_downloads<'e, E>(exec: E, file_path: &str) -> Result<Option<Uuid>>
where
    E: Executor<'e, Database = Postgres>,
{
    let download_id = sqlx::query_scalar!(
        r#"
        SELECT id FROM downloads
        WHERE file_path = $1 AND status != 'failed'
        LIMIT 1
        "#,
        file_path,
    )
    .fetch_optional(exec)
    .await?;
    Ok(download_id)
}
