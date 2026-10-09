//! Download lifecycle: completion, import, reingest, events, file access.

use super::super::Result;
use sqlx::{Executor, Postgres};
use uuid::Uuid;

/// Callback info returned after completing a download.
#[derive(Debug)]
pub struct CompletionCallbackInfo {
    pub callback_url: String,
    pub metadata: serde_json::Value,
    pub auth_token: Option<String>,
}

/// Update download status for completion and return callback info.
pub async fn complete_download_status<'e, E>(
    exec: E,
    id: Uuid,
    status: &str,
    file_path: Option<&str>,
) -> Result<Option<CompletionCallbackInfo>>
where
    E: Executor<'e, Database = Postgres>,
{
    let progress = if status == "completed" {
        Some(1.0)
    } else {
        None
    };
    let row = sqlx::query_as!(
        CompletionCallbackInfo,
        r#"
        UPDATE downloads
        SET status = $1, file_path = $2, progress = COALESCE($3, progress), updated_at = NOW()
        WHERE id = $4
        RETURNING callback_url, metadata, auth_token
        "#,
        status,
        file_path as Option<&str>,
        progress as Option<f64>,
        id,
    )
    .fetch_optional(exec)
    .await?;

    Ok(row)
}

/// Import confirmation result with client info for deletion.
#[derive(Debug)]
pub struct ImportConfirmationInfo {
    pub download_client_id: Option<Uuid>,
    pub external_id: Option<String>,
    pub protocol: String,
    pub client_data: Option<serde_json::Value>,
}

/// Check whether a download has already been confirmed as imported.
///
/// Used by failure handlers to avoid overwriting a successful import with a
/// late-arriving failure message (NATS redelivery race).
pub async fn is_import_confirmed<'e, E>(exec: E, id: Uuid) -> Result<bool>
where
    E: Executor<'e, Database = Postgres>,
{
    let confirmed: bool = sqlx::query_scalar!(
        "SELECT import_confirmed_at IS NOT NULL as \"confirmed!\" FROM downloads WHERE id = $1",
        id,
    )
    .fetch_optional(exec)
    .await?
    .unwrap_or(false);

    Ok(confirmed)
}

/// Confirm import and return client info for potential deletion.
///
/// Only updates if the download hasn't already been confirmed
/// (`import_confirmed_at IS NULL`), making this idempotent. Returns `None`
/// if already confirmed or not found — the caller skips cleanup in both cases.
///
/// A `failed`/`cancelled` download is never confirmed. Content validation can
/// fail a download microseconds before a late import confirmation arrives, and
/// confirming it anyway stamped `import_confirmed_at` on the watch match — which
/// `episode_has_imported_download` reads as "this episode is satisfied", silently
/// suppressing the re-search that the validation failure had just triggered. The
/// status test lives in this `WHERE` so the check and the write stay atomic.
pub async fn confirm_import<'e, E>(exec: E, id: Uuid) -> Result<Option<ImportConfirmationInfo>>
where
    E: Executor<'e, Database = Postgres>,
{
    let row = sqlx::query_as!(
        ImportConfirmationInfo,
        r#"
        UPDATE downloads
        SET import_confirmed_at = NOW(), updated_at = NOW()
        WHERE id = $1 AND import_confirmed_at IS NULL
          AND status NOT IN ('failed', 'cancelled')
        RETURNING download_client_id, external_id, protocol, client_data
        "#,
        id,
    )
    .fetch_optional(exec)
    .await?;

    Ok(row)
}

/// Reingest info for triggering callback.
#[derive(Debug)]
pub struct ReingestInfo {
    pub status: String,
    pub callback_url: String,
    pub auth_token: Option<String>,
    pub metadata: serde_json::Value,
    pub file_path: Option<String>,
    pub files_json: Option<serde_json::Value>,
    pub error: Option<String>,
    pub release_interpretation_json: Option<serde_json::Value>,
    pub name: String,
    pub media_kind: Option<String>,
    pub release_payload: Option<serde_json::Value>,
}

/// Get download info for reingest.
pub async fn get_reingest_info<'e, E>(exec: E, id: Uuid) -> Result<Option<ReingestInfo>>
where
    E: Executor<'e, Database = Postgres>,
{
    let row = sqlx::query_as!(
        ReingestInfo,
        r#"
        SELECT status, callback_url, auth_token, metadata, file_path, files_json, error,
               release_interpretation_json, name, media_kind, release_payload
        FROM downloads WHERE id = $1
        "#,
        id,
    )
    .fetch_optional(exec)
    .await?;

    Ok(row)
}

/// Info needed for rescanning files of a completed download.
#[derive(Debug)]
pub struct RescanInfo {
    pub status: String,
    pub download_client_id: Option<Uuid>,
    pub external_id: Option<String>,
    pub protocol: String,
    pub name: String,
}

/// Get download info for file rescanning.
pub async fn get_rescan_info<'e, E>(exec: E, id: Uuid) -> Result<Option<RescanInfo>>
where
    E: Executor<'e, Database = Postgres>,
{
    let row = sqlx::query_as!(
        RescanInfo,
        r#"
        SELECT status, download_client_id, external_id, protocol, name
        FROM downloads WHERE id = $1
        "#,
        id,
    )
    .fetch_optional(exec)
    .await?;

    Ok(row)
}

/// Reset callback tracking for reingest.
pub async fn reset_callback_tracking<'e, E>(
    exec: E,
    id: Uuid,
    callback_url: &str,
    auth_token: Option<&str>,
) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    // A reingest re-attempts the import, so a `failed` status left over from the
    // previous attempt is stale — the files are on disk and about to be handed to
    // Kobbler again. Clearing it back to `completed` matters because
    // `confirm_import` refuses to confirm a failed download: without this, the
    // incoming confirmation for a successful reingest would be dropped, and the
    // episode would look permanently un-imported.
    sqlx::query!(
        r#"
        UPDATE downloads
        SET callback_url = $1, auth_token = $2, callback_sent_at = NULL, import_confirmed_at = NULL,
            status = CASE WHEN status = 'failed' THEN 'completed' ELSE status END,
            error = CASE WHEN status = 'failed' THEN NULL ELSE error END,
            updated_at = NOW()
        WHERE id = $3
        "#,
        callback_url,
        auth_token as Option<&str>,
        id,
    )
    .execute(exec)
    .await?;
    Ok(())
}

// =============================================================================
// Event Logging
// =============================================================================

/// Log a download event.
pub async fn log_event<'e, E>(
    exec: E,
    download_id: Uuid,
    event_type: &str,
    message: &str,
    details: Option<serde_json::Value>,
) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query!(
        "INSERT INTO download_events (download_id, event_type, message, details) VALUES ($1, $2, $3, $4)",
        download_id,
        event_type,
        message,
        details as Option<serde_json::Value>,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// List download events for a download.
pub async fn list_download_events<'e, E>(
    exec: E,
    download_id: Uuid,
) -> Result<Vec<crate::types::DownloadEvent>>
where
    E: Executor<'e, Database = Postgres>,
{
    let events = sqlx::query_as!(
        crate::types::DownloadEvent,
        r#"
        SELECT id, event_type, message, details, created_at
        FROM download_events
        WHERE download_id = $1
        ORDER BY created_at ASC
        "#,
        download_id,
    )
    .fetch_all(exec)
    .await?;
    Ok(events)
}

// =============================================================================
// File Access
// =============================================================================

/// Get file path for a download.
pub async fn get_file_path<'e, E>(exec: E, id: Uuid) -> Result<Option<Option<String>>>
where
    E: Executor<'e, Database = Postgres>,
{
    let row = sqlx::query!("SELECT file_path FROM downloads WHERE id = $1", id,)
        .fetch_optional(exec)
        .await?;

    Ok(row.map(|r| r.file_path))
}

/// Info about a file retrieved from files_json
#[derive(Debug)]
pub struct FileByIdInfo {
    pub absolute_path: String,
    pub split_rar_volumes: Option<Vec<String>>,
    /// Full paths to source archives for distributed split RARs.
    /// e.g., ["dir/a.zip!/file.rar", "dir/b.zip!/file.r00", ...]
    pub distributed_rar_sources: Option<Vec<String>>,
    /// Full paths for distributed numeric splits (.7z.001, .zip.001, etc.)
    pub distributed_numeric_sources: Option<Vec<String>>,
    /// Archive type for numeric splits (e.g., "7z", "zip")
    pub numeric_split_type: Option<String>,
}

/// Look up a file by ID in files_json and return its path info.
pub async fn get_file_by_id<'e, E>(
    exec: E,
    download_id: Uuid,
    file_id: Uuid,
) -> Result<Option<FileByIdInfo>>
where
    E: Executor<'e, Database = Postgres>,
{
    let file_id_str = file_id.to_string();
    let row = sqlx::query!(
        r#"
        SELECT elem->>'absolute_path' AS "absolute_path!",
               elem->'split_rar_volumes' AS split_rar_volumes,
               elem->'distributed_rar_sources' AS distributed_rar_sources,
               elem->'distributed_numeric_sources' AS distributed_numeric_sources,
               elem->>'numeric_split_type' AS numeric_split_type
        FROM downloads,
             jsonb_array_elements(files_json) AS elem
        WHERE downloads.id = $1
          AND elem->>'id' = $2
        LIMIT 1
        "#,
        download_id,
        file_id_str,
    )
    .fetch_optional(exec)
    .await?;

    Ok(row.map(|r| {
        // Parse JSON arrays to Vec<String>
        let parse_string_array = |v: serde_json::Value| {
            v.as_array().map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
        };

        let split_rar_volumes = r.split_rar_volumes.and_then(parse_string_array);
        let distributed_rar_sources = r.distributed_rar_sources.and_then(parse_string_array);
        let distributed_numeric_sources =
            r.distributed_numeric_sources.and_then(parse_string_array);

        FileByIdInfo {
            absolute_path: r.absolute_path,
            split_rar_volumes,
            distributed_rar_sources,
            distributed_numeric_sources,
            numeric_split_type: r.numeric_split_type,
        }
    }))
}
