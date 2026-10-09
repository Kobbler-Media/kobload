//! Download polling state: status updates, callbacks, files scan recovery.

use super::super::Result;
use super::DownloadStatusUpdate;
use sqlx::{Executor, Postgres};
use uuid::Uuid;

/// Update poll timestamp and error.
pub async fn update_poll_status<'e, E>(
    exec: E,
    id: Uuid,
    error: Option<&str>,
    progress: Option<f64>,
) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query!(
        "UPDATE downloads SET last_poll_at = NOW(), last_poll_error = $1, progress = $2 WHERE id = $3",
        error as Option<&str>,
        progress as Option<f64>,
        id,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Mark download as failed.
///
/// Skips the update if the download has already been confirmed as imported
/// (`import_confirmed_at IS NOT NULL`), preventing a late-arriving failure
/// from overwriting a successful import.
pub async fn mark_failed<'e, E>(exec: E, id: Uuid, error: &str) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query!(
        "UPDATE downloads SET status = 'failed', error = $1, last_poll_at = NOW(), last_poll_error = $1, progress = NULL, updated_at = NOW() WHERE id = $2 AND import_confirmed_at IS NULL",
        error,
        id,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Atomically mark a download failed, but only if it is not already failed.
///
/// Returns `true` if this call performed the transition, `false` if the
/// download was already failed (or already imported, or gone). Import-failure
/// handlers use this to make re-search fire exactly once: kobbler can report
/// `import_failed` more than once for the same download, and without this every
/// repeat spawned another re-search, forking one replacement chain into several
/// that each grabbed their own copy.
pub async fn claim_failure<'e, E>(exec: E, id: Uuid, error: &str) -> Result<bool>
where
    E: Executor<'e, Database = Postgres>,
{
    let claimed = sqlx::query_scalar!(
        r#"UPDATE downloads
           SET status = 'failed', error = $1, last_poll_at = NOW(), last_poll_error = $1,
               progress = NULL, updated_at = NOW()
           WHERE id = $2 AND import_confirmed_at IS NULL AND status <> 'failed'
           RETURNING id"#,
        error,
        id,
    )
    .fetch_optional(exec)
    .await?
    .is_some();

    Ok(claimed)
}

/// Update download status and related fields after poll.
pub async fn update_status<'e, E>(exec: E, id: Uuid, input: &DownloadStatusUpdate<'_>) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query!(
        r#"UPDATE downloads SET
            status = $1,
            file_path = COALESCE($2, file_path),
            error = $3,
            progress = COALESCE($4, progress),
            files_scan_error = $5,
            client_data = COALESCE($6, client_data),
            updated_at = NOW()
        WHERE id = $7"#,
        input.status,
        input.file_path as Option<&str>,
        input.error as Option<&str>,
        input.progress as Option<f64>,
        input.files_scan_error as Option<&str>,
        input.client_data as Option<&serde_json::Value>,
        id,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Store scanned files JSON.
pub async fn update_files_json<'e, E>(
    exec: E,
    id: Uuid,
    files_json: &serde_json::Value,
) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query!(
        "UPDATE downloads SET files_json = $1 WHERE id = $2",
        files_json,
        id,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Store enriched files_json and release_interpretation_json after metadata extraction.
pub async fn update_enriched_metadata<'e, E>(
    exec: E,
    id: Uuid,
    files_json: &serde_json::Value,
    release_interpretation_json: Option<&serde_json::Value>,
) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query!(
        r#"UPDATE downloads SET
            files_json = $1,
            release_interpretation_json = $2,
            updated_at = NOW()
        WHERE id = $3"#,
        files_json,
        release_interpretation_json as Option<&serde_json::Value>,
        id,
    )
    .execute(exec)
    .await?;
    Ok(())
}

// =============================================================================
// Callback Updates
// =============================================================================

/// Update callback attempt tracking.
pub async fn update_callback_attempt<'e, E>(exec: E, id: Uuid, attempts: i32) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query!(
        "UPDATE downloads SET callback_attempts = $1, callback_last_attempt_at = NOW() WHERE id = $2",
        attempts,
        id,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Mark callback as sent successfully.
pub async fn mark_callback_sent<'e, E>(exec: E, id: Uuid) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query!(
        "UPDATE downloads SET callback_sent_at = NOW() WHERE id = $1",
        id,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Update callback error.
pub async fn update_callback_error<'e, E>(exec: E, id: Uuid, error: &str) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query!(
        "UPDATE downloads SET callback_error = $1 WHERE id = $2",
        error,
        id,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Update callback_last_attempt_at only (for backoff).
pub async fn touch_callback_attempt<'e, E>(exec: E, id: Uuid) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query!(
        "UPDATE downloads SET callback_last_attempt_at = NOW() WHERE id = $1",
        id,
    )
    .execute(exec)
    .await?;
    Ok(())
}

// =============================================================================
// Files Scan Recovery
// =============================================================================

/// Clear files_scan_error and reset callback state after successful recovery.
pub async fn clear_files_scan_error<'e, E>(
    exec: E,
    id: Uuid,
    file_path: Option<&str>,
    files_json: Option<&serde_json::Value>,
) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query!(
        r#"UPDATE downloads SET
            files_scan_error = NULL,
            file_path = COALESCE($1, file_path),
            files_json = COALESCE($2, files_json),
            callback_sent_at = NULL,
            callback_attempts = 0,
            updated_at = NOW()
        WHERE id = $3"#,
        file_path as Option<&str>,
        files_json as Option<&serde_json::Value>,
        id,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Update files_scan_error after retry failure.
pub async fn update_files_scan_error<'e, E>(exec: E, id: Uuid, error: &str) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query!(
        "UPDATE downloads SET files_scan_error = $1, callback_last_attempt_at = NOW() WHERE id = $2",
        error,
        id,
    )
    .execute(exec)
    .await?;
    Ok(())
}

// =============================================================================
// Submission Retry
// =============================================================================

/// Update submission retry tracking after an attempt.
pub async fn update_submission_attempt<'e, E>(
    exec: E,
    id: Uuid,
    attempts: i32,
    error: Option<&str>,
) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query!(
        r#"UPDATE downloads SET
            submission_attempts = $1,
            submission_last_attempt_at = NOW(),
            submission_error = $2
        WHERE id = $3"#,
        attempts,
        error as Option<&str>,
        id,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Mark a pending_retry download as successfully submitted.
pub async fn mark_submission_success<'e, E>(exec: E, id: Uuid, external_id: &str) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query!(
        r#"UPDATE downloads SET
            status = 'downloading',
            external_id = $1,
            submission_error = NULL,
            updated_at = NOW()
        WHERE id = $2"#,
        external_id,
        id,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Mark a pending_retry download as permanently failed after max attempts.
pub async fn mark_submission_failed<'e, E>(exec: E, id: Uuid, error: &str) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query!(
        r#"UPDATE downloads SET
            status = 'failed',
            error = $1,
            updated_at = NOW()
        WHERE id = $2"#,
        error,
        id,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Finalize a reserved ('submitting') download after the client add resolves: set the
/// real status, external_id, error/submission_error, progress and the final download_url
/// (which `prepare_download` may have rewritten, e.g. MAM freeleech wedge) in one
/// statement. dedup_key is intentionally left as the pre-prepare value reserved earlier.
#[allow(clippy::too_many_arguments)]
pub async fn finalize_submission<'e, E>(
    exec: E,
    id: Uuid,
    status: &str,
    external_id: Option<&str>,
    error: Option<&str>,
    submission_error: Option<&str>,
    progress: Option<f64>,
    download_url: &str,
) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query!(
        r#"UPDATE downloads SET
            status = $1,
            external_id = $2,
            error = $3,
            submission_error = $4,
            progress = $5,
            download_url = $6,
            updated_at = NOW()
        WHERE id = $7"#,
        status,
        external_id as Option<&str>,
        error as Option<&str>,
        submission_error as Option<&str>,
        progress as Option<f64>,
        download_url,
        id,
    )
    .execute(exec)
    .await?;
    Ok(())
}

// =============================================================================
// Callback Idempotency
// =============================================================================

/// Get or create an idempotency key for a download's callback.
///
/// On first callback attempt, generates a new UUID and stores it.
/// On retries, returns the existing key for deduplication.
pub async fn get_or_create_idempotency_key<'e, E>(exec: E, id: Uuid) -> Result<Uuid>
where
    E: Executor<'e, Database = Postgres>,
{
    // Use INSERT ... ON CONFLICT to atomically get or create
    // This handles the race condition where two threads try to create at once
    let new_key = Uuid::now_v7();
    let key: Uuid = sqlx::query_scalar!(
        r#"
        UPDATE downloads
        SET callback_idempotency_key = COALESCE(callback_idempotency_key, $2)
        WHERE id = $1
        RETURNING callback_idempotency_key AS "callback_idempotency_key!"
        "#,
        id,
        new_key,
    )
    .fetch_one(exec)
    .await?;
    Ok(key)
}
