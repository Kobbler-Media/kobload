//! Download CRUD and list operations.

use super::super::Result;
use super::{DownloadInput, PendingDownload};
use crate::types::{DownloadDetail, DownloadListItem};
use sqlx::{Executor, Postgres};
use time::OffsetDateTime;
use uuid::Uuid;

/// Pagination options for download list.
#[derive(Debug, Default)]
pub struct DownloadListOptions {
    pub limit: i64,
    pub cursor: Option<String>,
    pub status: Option<Vec<String>>,
    pub search: Option<String>,
}

/// Response for paginated download list.
#[derive(Debug)]
pub struct DownloadListResponse {
    pub items: Vec<DownloadListItem>,
    pub next_cursor: Option<String>,
}

/// Encode a cursor from (created_at, id).
fn encode_cursor(created_at: OffsetDateTime, id: Uuid) -> String {
    // Format: timestamp_nanos:uuid
    let ts = created_at.unix_timestamp_nanos();
    format!("{}:{}", ts, id)
}

/// Decode a cursor into (created_at, id).
fn decode_cursor(cursor: &str) -> Option<(OffsetDateTime, Uuid)> {
    let parts: Vec<&str> = cursor.split(':').collect();
    if parts.len() != 2 {
        return None;
    }
    let ts: i128 = parts[0].parse().ok()?;
    let id: Uuid = parts[1].parse().ok()?;
    let created_at = OffsetDateTime::from_unix_timestamp_nanos(ts).ok()?;
    Some((created_at, id))
}

/// Download deletion info.
#[derive(Debug)]
pub struct DownloadDeletionInfo {
    pub download_client_id: Option<Uuid>,
    pub external_id: Option<String>,
    pub protocol: String,
    pub status: String,
}

#[derive(Debug)]
pub struct InactiveDeletionInfo {
    pub id: Uuid,
    pub download_client_id: Option<Uuid>,
    pub external_id: Option<String>,
    pub protocol: String,
}

/// Create a new download.
pub async fn create_download<'e, E>(exec: E, input: DownloadInput) -> Result<Uuid>
where
    E: Executor<'e, Database = Postgres>,
{
    let id = Uuid::now_v7();
    sqlx::query!(
        r#"
        INSERT INTO downloads (
            id, source, protocol, profile_id, name, download_url, download_client_id, external_id,
            callback_url, auth_token, metadata, status, progress, error, release_payload, api_key_id,
            quality_profile_id, media_kind, submission_error,
            created_at, updated_at
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, NOW(), NOW())
        "#,
        id,
        &input.source,
        &input.protocol,
        input.profile_id as Option<Uuid>,
        &input.name,
        &input.download_url,
        input.download_client_id,
        input.external_id.as_deref() as Option<&str>,
        input.callback_url.as_deref() as Option<&str>,
        input.auth_token.as_deref() as Option<&str>,
        input.metadata as Option<serde_json::Value>,
        &input.status,
        input.progress as Option<f64>,
        input.error.as_deref() as Option<&str>,
        input.release_payload as Option<serde_json::Value>,
        input.api_key_id as Option<Uuid>,
        input.quality_profile_id as Option<Uuid>,
        input.media_kind.as_deref() as Option<&str>,
        input.submission_error.as_deref() as Option<&str>,
    )
    .execute(exec)
    .await?;
    Ok(id)
}

/// Reserve a download URL before submitting to the client. Inserts the row with the
/// given `dedup_key`; the partial unique index `downloads_dedup_key_active` rejects the
/// insert (returning `None`) if another in-flight download already holds the key. This
/// closes the ~1s race two watch patterns can otherwise win together. A NULL `dedup_key`
/// (manual/user downloads) never conflicts, so those always insert.
pub async fn reserve_download<'e, E>(
    exec: E,
    input: DownloadInput,
    dedup_key: Option<&str>,
) -> Result<Option<Uuid>>
where
    E: Executor<'e, Database = Postgres>,
{
    let id = Uuid::now_v7();
    let row = sqlx::query_scalar!(
        r#"
        INSERT INTO downloads (
            id, source, protocol, profile_id, name, download_url, download_client_id, external_id,
            callback_url, auth_token, metadata, status, progress, error, release_payload, api_key_id,
            quality_profile_id, media_kind, submission_error, dedup_key,
            created_at, updated_at
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18, $19, $20, NOW(), NOW())
        ON CONFLICT (dedup_key)
            WHERE dedup_key IS NOT NULL
              AND status IN ('submitting', 'queued', 'downloading', 'pending_retry')
            DO NOTHING
        RETURNING id
        "#,
        id,
        &input.source,
        &input.protocol,
        input.profile_id as Option<Uuid>,
        &input.name,
        &input.download_url,
        input.download_client_id,
        input.external_id.as_deref() as Option<&str>,
        input.callback_url.as_deref() as Option<&str>,
        input.auth_token.as_deref() as Option<&str>,
        input.metadata as Option<serde_json::Value>,
        &input.status,
        input.progress as Option<f64>,
        input.error.as_deref() as Option<&str>,
        input.release_payload as Option<serde_json::Value>,
        input.api_key_id as Option<Uuid>,
        input.quality_profile_id as Option<Uuid>,
        input.media_kind.as_deref() as Option<&str>,
        input.submission_error.as_deref() as Option<&str>,
        dedup_key as Option<&str>,
    )
    .fetch_optional(exec)
    .await?;
    Ok(row)
}

/// An existing download that covers a (re)submission for the same URL.
#[derive(Debug)]
pub struct RecentDownloadByUrl {
    pub id: Uuid,
    pub status: String,
    pub external_id: Option<String>,
}

/// Find the most recent non-failed/non-cancelled download for `download_url`
/// created within the last `window_hours`. Used by `submit_download` to suppress
/// duplicate grabs of the same release (two watch patterns firing ~1s apart, or
/// a later `want` re-grabbing a release that already imported). Failed/cancelled
/// rows are ignored so a genuine retry after failure still proceeds.
pub async fn find_recent_active_download_by_url<'e, E>(
    exec: E,
    download_url: &str,
    window_hours: i32,
) -> Result<Option<RecentDownloadByUrl>>
where
    E: Executor<'e, Database = Postgres>,
{
    let row = sqlx::query_as!(
        RecentDownloadByUrl,
        r#"
        SELECT id, status, external_id
        FROM downloads
        WHERE download_url = $1
          AND status NOT IN ('failed', 'cancelled')
          AND created_at > NOW() - make_interval(hours => $2)
        ORDER BY created_at DESC
        LIMIT 1
        "#,
        download_url,
        window_hours,
    )
    .fetch_optional(exec)
    .await?;
    Ok(row)
}

/// Get a download by ID (detail view).
pub async fn get_download_detail<'e, E>(exec: E, id: Uuid) -> Result<Option<DownloadDetail>>
where
    E: Executor<'e, Database = Postgres>,
{
    let row = sqlx::query_as!(
        DownloadDetail,
        r#"
        SELECT id, source, protocol, profile_id, name, download_url, callback_url,
               download_client_id, external_id, metadata, status, progress, file_path, error, release_payload,
               created_at, updated_at
        FROM downloads
        WHERE id = $1
        "#,
        id,
    )
    .fetch_optional(exec)
    .await?;
    Ok(row)
}

/// Get download info needed for deletion.
pub async fn get_deletion_info<'e, E>(exec: E, id: Uuid) -> Result<Option<DownloadDeletionInfo>>
where
    E: Executor<'e, Database = Postgres>,
{
    let row = sqlx::query_as!(
        DownloadDeletionInfo,
        r#"
        SELECT download_client_id, external_id, protocol, status
        FROM downloads WHERE id = $1
        "#,
        id,
    )
    .fetch_optional(exec)
    .await?;

    Ok(row)
}

/// Soft-delete a download (hide from UI, keep for watch system).
pub async fn delete_download<'e, E>(exec: E, id: Uuid) -> Result<bool>
where
    E: Executor<'e, Database = Postgres>,
{
    let result = sqlx::query!(
        "UPDATE downloads SET hidden = TRUE, updated_at = NOW() WHERE id = $1 AND NOT hidden",
        id,
    )
    .execute(exec)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Soft-delete all non-active downloads (everything except queued/downloading).
pub async fn clear_downloads<'e, E>(exec: E) -> Result<u64>
where
    E: Executor<'e, Database = Postgres>,
{
    let result = sqlx::query!(
        "UPDATE downloads SET hidden = TRUE, updated_at = NOW() WHERE status NOT IN ('queued', 'downloading') AND NOT hidden"
    )
    .execute(exec)
    .await?;
    Ok(result.rows_affected())
}

/// Fetch deletion info for all inactive downloads that have client info.
pub async fn get_inactive_deletion_info<'e, E>(exec: E) -> Result<Vec<InactiveDeletionInfo>>
where
    E: Executor<'e, Database = Postgres>,
{
    let rows = sqlx::query_as!(
        InactiveDeletionInfo,
        r#"
        SELECT id, download_client_id, external_id, protocol
        FROM downloads
        WHERE status NOT IN ('queued', 'downloading')
          AND NOT hidden
          AND download_client_id IS NOT NULL
          AND external_id IS NOT NULL
        "#,
    )
    .fetch_all(exec)
    .await?;

    Ok(rows)
}

/// Fetch download list with joined data and pagination.
pub async fn fetch_download_list<'e, E>(
    exec: E,
    options: DownloadListOptions,
) -> Result<DownloadListResponse>
where
    E: Executor<'e, Database = Postgres>,
{
    let limit = options.limit.clamp(1, 100);
    // Fetch one extra to determine if there's a next page
    let fetch_limit = limit + 1;

    // Build query based on cursor and status filter
    let (cursor_ts, cursor_id) = options
        .cursor
        .as_ref()
        .and_then(|c| decode_cursor(c))
        .unzip();

    let status_filter = options.status.as_ref();
    let search_filter = options.search.as_deref();

    let items = sqlx::query_as!(
        DownloadListItem,
        r#"
        SELECT d.id, d.name, d.protocol, d.profile_id, d.status, d.progress,
               d.last_poll_at, d.last_poll_error, d.created_at, d.updated_at,
               d.download_client_id, d.external_id, d.file_path, d.error,
               d.callback_sent_at, d.import_confirmed_at, d.files_json, d.release_payload,
               d.release_interpretation_json,
               d.metadata,
               d.callback_attempts, d.callback_last_attempt_at, d.callback_error,
               d.files_scan_error, d.client_data AS client_progress,
               dc.name AS "download_client_name?",
               d.api_key_id,
               ak.name AS "api_key_name?",
               wp.source_system AS "watch_pattern_source_system?",
               wp.source_id AS "watch_pattern_source_id?"
        FROM downloads d
        LEFT JOIN download_clients dc ON d.download_client_id = dc.id
        LEFT JOIN api_keys ak ON d.api_key_id = ak.id
        LEFT JOIN LATERAL (
            SELECT wm.watch_pattern_id
            FROM watch_matches wm
            WHERE wm.download_id = d.id
            LIMIT 1
        ) wm ON true
        LEFT JOIN watch_patterns wp ON wm.watch_pattern_id = wp.id
        WHERE NOT d.hidden
          AND ($1::timestamptz IS NULL OR (d.created_at, d.id) < ($1, $2))
          AND ($3::text[] IS NULL OR d.status = ANY($3))
          AND ($5::text IS NULL OR d.name ILIKE '%' || $5 || '%')
        ORDER BY d.created_at DESC, d.id DESC
        LIMIT $4
        "#,
        cursor_ts as Option<OffsetDateTime>,
        cursor_id as Option<Uuid>,
        status_filter as Option<&Vec<String>>,
        fetch_limit,
        search_filter,
    )
    .fetch_all(exec)
    .await?;

    // Determine next cursor
    let has_more = items.len() as i64 > limit;
    let items: Vec<_> = items.into_iter().take(limit as usize).collect();

    let next_cursor = if has_more {
        items
            .last()
            .map(|item| encode_cursor(item.created_at, item.id))
    } else {
        None
    };

    Ok(DownloadListResponse { items, next_cursor })
}

/// Fetch a single download by ID with joined data.
pub async fn fetch_download_by_id<'e, E>(exec: E, id: Uuid) -> Result<Option<DownloadListItem>>
where
    E: Executor<'e, Database = Postgres>,
{
    let item = sqlx::query_as!(
        DownloadListItem,
        r#"
        SELECT d.id, d.name, d.protocol, d.profile_id, d.status, d.progress,
               d.last_poll_at, d.last_poll_error, d.created_at, d.updated_at,
               d.download_client_id, d.external_id, d.file_path, d.error,
               d.callback_sent_at, d.import_confirmed_at, d.files_json, d.release_payload,
               d.release_interpretation_json,
               d.metadata,
               d.callback_attempts, d.callback_last_attempt_at, d.callback_error,
               d.files_scan_error, d.client_data AS client_progress,
               dc.name AS "download_client_name?",
               d.api_key_id,
               ak.name AS "api_key_name?",
               wp.source_system AS "watch_pattern_source_system?",
               wp.source_id AS "watch_pattern_source_id?"
        FROM downloads d
        LEFT JOIN download_clients dc ON d.download_client_id = dc.id
        LEFT JOIN api_keys ak ON d.api_key_id = ak.id
        LEFT JOIN LATERAL (
            SELECT wm.watch_pattern_id
            FROM watch_matches wm
            WHERE wm.download_id = d.id
            LIMIT 1
        ) wm ON true
        LEFT JOIN watch_patterns wp ON wm.watch_pattern_id = wp.id
        WHERE d.id = $1
        "#,
        id,
    )
    .fetch_optional(exec)
    .await?;
    Ok(item)
}

/// Fetch pending downloads for polling.
pub async fn fetch_pending_downloads<'e, E>(exec: E) -> Result<Vec<PendingDownload>>
where
    E: Executor<'e, Database = Postgres>,
{
    let downloads = sqlx::query_as!(
        PendingDownload,
        r#"
        SELECT id, download_client_id, external_id, status, protocol, name, callback_url, auth_token, metadata,
               callback_sent_at, file_path, error, callback_attempts, files_scan_error, files_json, release_payload,
               media_kind, callback_idempotency_key
        FROM downloads
        WHERE status IN ('queued', 'downloading')
           OR (status IN ('completed', 'failed') AND (
               -- Needs callback: not sent yet, and either:
               -- 1. Never attempted OR
               -- 2. Under max attempts AND enough time has passed (exponential backoff)
               -- Schedule: 0, 1min, 2min, 5min, 10min, 15min, 30min, 45min, 60min, 120min (10 attempts)
               (callback_sent_at IS NULL AND callback_attempts < 10 AND (
                   callback_last_attempt_at IS NULL OR
                   callback_last_attempt_at < NOW() - (
                       CASE callback_attempts
                           WHEN 0 THEN INTERVAL '0 seconds'
                           WHEN 1 THEN INTERVAL '1 minute'
                           WHEN 2 THEN INTERVAL '2 minutes'
                           WHEN 3 THEN INTERVAL '5 minutes'
                           WHEN 4 THEN INTERVAL '10 minutes'
                           WHEN 5 THEN INTERVAL '15 minutes'
                           WHEN 6 THEN INTERVAL '30 minutes'
                           WHEN 7 THEN INTERVAL '45 minutes'
                           WHEN 8 THEN INTERVAL '60 minutes'
                           WHEN 9 THEN INTERVAL '120 minutes'
                           ELSE INTERVAL '999 years'
                       END
                   )
               ))
               -- OR: Has files_scan_error that should be retried (every minute)
               OR (files_scan_error IS NOT NULL AND files_scan_error != '' AND (
                   callback_last_attempt_at IS NULL OR
                   callback_last_attempt_at < NOW() - INTERVAL '1 minute'
               ))
           ))
        "#,
    )
    .fetch_all(exec)
    .await?;
    Ok(downloads)
}

/// Fetch a single pending download by ID (for job handler use).
pub async fn fetch_pending_download_by_id<'e, E>(
    exec: E,
    id: Uuid,
) -> Result<Option<PendingDownload>>
where
    E: Executor<'e, Database = Postgres>,
{
    let download = sqlx::query_as!(
        PendingDownload,
        r#"
        SELECT id, download_client_id, external_id, status, protocol, name, callback_url, auth_token, metadata,
               callback_sent_at, file_path, error, callback_attempts, files_scan_error, files_json, release_payload,
               media_kind, callback_idempotency_key
        FROM downloads
        WHERE id = $1
        "#,
        id,
    )
    .fetch_optional(exec)
    .await?;
    Ok(download)
}

/// Download needing submission retry.
#[derive(Debug)]
pub struct PendingRetryDownload {
    pub id: Uuid,
    pub download_client_id: Uuid,
    pub download_url: String,
    pub protocol: String,
    pub name: String,
    pub callback_url: Option<String>,
    pub auth_token: Option<String>,
    pub metadata: Option<serde_json::Value>,
    pub release_payload: Option<serde_json::Value>,
    pub submission_attempts: i32,
    pub submission_error: Option<String>,
}

/// Fetch downloads needing submission retry.
/// Uses same exponential backoff schedule as callback retries.
pub async fn fetch_pending_retry_downloads<'e, E>(exec: E) -> Result<Vec<PendingRetryDownload>>
where
    E: Executor<'e, Database = Postgres>,
{
    let downloads = sqlx::query_as!(
        PendingRetryDownload,
        r#"
        SELECT id, download_client_id as "download_client_id!", download_url, protocol, name, callback_url, auth_token,
               metadata, release_payload, submission_attempts, submission_error
        FROM downloads
        WHERE status = 'pending_retry'
          AND submission_attempts < 10
          AND (
            submission_last_attempt_at IS NULL OR
            submission_last_attempt_at < NOW() - (
                CASE submission_attempts
                    WHEN 1 THEN INTERVAL '1 minute'
                    WHEN 2 THEN INTERVAL '2 minutes'
                    WHEN 3 THEN INTERVAL '5 minutes'
                    WHEN 4 THEN INTERVAL '10 minutes'
                    WHEN 5 THEN INTERVAL '15 minutes'
                    WHEN 6 THEN INTERVAL '30 minutes'
                    WHEN 7 THEN INTERVAL '45 minutes'
                    WHEN 8 THEN INTERVAL '60 minutes'
                    WHEN 9 THEN INTERVAL '120 minutes'
                    ELSE INTERVAL '999 years'
                END
            )
          )
        "#,
    )
    .fetch_all(exec)
    .await?;
    Ok(downloads)
}

/// Get download IDs referenced by a watch pattern's matches.
///
/// Used before pattern deletion to collect IDs for orphan cleanup afterward.
pub async fn get_download_ids_for_pattern<'e, E>(exec: E, pattern_id: Uuid) -> Result<Vec<Uuid>>
where
    E: Executor<'e, Database = Postgres>,
{
    let ids = sqlx::query_scalar!(
        "SELECT DISTINCT download_id FROM watch_matches WHERE watch_pattern_id = $1 AND download_id IS NOT NULL",
        pattern_id,
    )
    .fetch_all(exec)
    .await?;
    // query_scalar returns Option<Uuid> due to nullability inference; filter
    Ok(ids.into_iter().flatten().collect())
}

/// Get download IDs referenced by any of the given watch patterns.
pub async fn get_download_ids_for_patterns<'e, E>(
    exec: E,
    pattern_ids: &[Uuid],
) -> Result<Vec<Uuid>>
where
    E: Executor<'e, Database = Postgres>,
{
    let ids = sqlx::query_scalar!(
        "SELECT DISTINCT download_id FROM watch_matches WHERE watch_pattern_id = ANY($1) AND download_id IS NOT NULL",
        pattern_ids,
    )
    .fetch_all(exec)
    .await?;
    Ok(ids.into_iter().flatten().collect())
}

/// Hard-delete hidden downloads that are no longer referenced by any watch match.
///
/// Called after pattern deletion (which CASCADE-deletes matches) to clean up
/// orphaned hidden downloads that no longer serve any dedup purpose.
pub async fn hard_delete_orphaned_hidden_downloads<'e, E>(exec: E, ids: &[Uuid]) -> Result<u64>
where
    E: Executor<'e, Database = Postgres>,
{
    if ids.is_empty() {
        return Ok(0);
    }
    let result = sqlx::query!(
        r#"DELETE FROM downloads
        WHERE id = ANY($1)
          AND hidden = TRUE
          AND NOT EXISTS (SELECT 1 FROM watch_matches WHERE download_id = downloads.id)"#,
        ids,
    )
    .execute(exec)
    .await?;
    Ok(result.rows_affected())
}
