//! Watch matches: CRUD operations.

use super::super::Result;
use sqlx::{Executor, Postgres};
use time::OffsetDateTime;
use uuid::Uuid;

// =============================================================================
// Cursor Pagination
// =============================================================================

/// Pagination options for watch match list.
#[derive(Debug, Default)]
pub struct WatchMatchListOptions {
    pub limit: i64,
    pub cursor: Option<OffsetDateTime>,
    pub cursor_id: Option<Uuid>,
    pub watch_pattern_id: Option<Uuid>,
    pub action: Option<String>,
    pub rejected: Option<bool>,
    pub search: Option<String>,
}

/// Response for paginated watch match list.
#[derive(Debug)]
pub struct WatchMatchListResponse {
    pub items: Vec<WatchMatchRow>,
    pub next_cursor: Option<String>,
}

/// Encode a cursor from (created_at, id).
fn encode_cursor(created_at: OffsetDateTime, id: Uuid) -> String {
    let ts = created_at.unix_timestamp_nanos();
    format!("{}:{}", ts, id)
}

/// Decode a cursor into (created_at, id).
pub fn decode_cursor(cursor: &str) -> Option<(OffsetDateTime, Uuid)> {
    let parts: Vec<&str> = cursor.split(':').collect();
    if parts.len() != 2 {
        return None;
    }
    let ts: i128 = parts[0].parse().ok()?;
    let id: Uuid = parts[1].parse().ok()?;
    let created_at = OffsetDateTime::from_unix_timestamp_nanos(ts).ok()?;
    Some((created_at, id))
}

/// Fetch watch match list with cursor pagination and optional filters.
pub async fn fetch_match_list<'e, E>(
    exec: E,
    options: WatchMatchListOptions,
) -> Result<WatchMatchListResponse>
where
    E: Executor<'e, Database = Postgres>,
{
    let limit = options.limit.clamp(1, 100);
    let fetch_limit = limit + 1;

    let items = sqlx::query_as!(
        WatchMatchRow,
        r#"SELECT id,
           watch_pattern_id as "watch_pattern_id!",
           indexer_source_id,
           release_guid,
           release_title,
           release_data as "release_data?: serde_json::Value",
           match_score,
           matched_pattern as "matched_pattern!",
           action,
           download_id,
           callback_sent_at,
           callback_error,
           created_at as "created_at!",
           hold_until,
           held,
           callback_attempts,
           callback_last_attempt_at,
           quality_score,
           rejected,
           rejected_at,
           rejected_reason,
           import_confirmed_at,
           episode_key
        FROM watch_matches
        WHERE NOT hidden
          AND ($1::timestamptz IS NULL OR (created_at, id) < ($1, $2))
          AND ($3::uuid IS NULL OR watch_pattern_id = $3)
          AND ($4::text IS NULL OR action = $4)
          AND ($5::bool IS NULL OR rejected = $5)
          AND ($7::text IS NULL OR release_title ILIKE '%' || $7 || '%')
        ORDER BY created_at DESC, id DESC
        LIMIT $6"#,
        options.cursor as Option<OffsetDateTime>,
        options.cursor_id as Option<Uuid>,
        options.watch_pattern_id as Option<Uuid>,
        options.action.as_deref() as Option<&str>,
        options.rejected as Option<bool>,
        fetch_limit,
        options.search.as_deref() as Option<&str>,
    )
    .fetch_all(exec)
    .await?;

    let has_more = items.len() as i64 > limit;
    let items: Vec<_> = items.into_iter().take(limit as usize).collect();

    let next_cursor = if has_more {
        items
            .last()
            .map(|item| encode_cursor(item.created_at, item.id))
    } else {
        None
    };

    Ok(WatchMatchListResponse { items, next_cursor })
}

/// Watch match row from database.
#[derive(Debug, Clone)]
pub struct WatchMatchRow {
    pub id: Uuid,
    pub watch_pattern_id: Uuid,
    pub indexer_source_id: Option<Uuid>,
    pub release_guid: String,
    pub release_title: String,
    pub release_data: Option<serde_json::Value>,
    pub match_score: f64,
    pub matched_pattern: String,
    pub action: String,
    pub download_id: Option<Uuid>,
    pub callback_sent_at: Option<OffsetDateTime>,
    pub callback_error: Option<String>,
    pub created_at: OffsetDateTime,
    pub hold_until: Option<OffsetDateTime>,
    pub held: bool,
    pub callback_attempts: i32,
    pub callback_last_attempt_at: Option<OffsetDateTime>,
    pub quality_score: Option<f64>,
    pub rejected: bool,
    pub rejected_at: Option<OffsetDateTime>,
    pub rejected_reason: Option<String>,
    pub import_confirmed_at: Option<OffsetDateTime>,
    pub episode_key: Option<String>,
}

/// Input for creating a watch match.
#[derive(Debug)]
pub struct WatchMatchInput<'a> {
    pub pattern_id: Uuid,
    pub indexer_source_id: Option<Uuid>,
    pub release_guid: &'a str,
    pub release_title: &'a str,
    pub release_data: Option<&'a serde_json::Value>,
    pub match_score: f64,
    pub matched_pattern: &'a str,
    pub action: &'a str,
    pub download_id: Option<Uuid>,
    pub hold_until: Option<OffsetDateTime>,
    pub held: bool,
    pub quality_score: Option<f64>,
    pub rejected: bool,
    pub rejected_at: Option<OffsetDateTime>,
    pub rejected_reason: Option<&'a str>,
    pub episode_key: Option<&'a str>,
}

/// List watch matches.
pub async fn list_matches<'e, E>(exec: E, limit: i64) -> Result<Vec<WatchMatchRow>>
where
    E: Executor<'e, Database = Postgres>,
{
    let rows = sqlx::query_as!(
        WatchMatchRow,
        r#"SELECT id,
           watch_pattern_id as "watch_pattern_id!",
           indexer_source_id,
           release_guid,
           release_title,
           release_data as "release_data?: serde_json::Value",
           match_score,
           matched_pattern as "matched_pattern!",
           action,
           download_id,
           callback_sent_at,
           callback_error,
           created_at as "created_at!",
           hold_until,
           held,
           callback_attempts,
           callback_last_attempt_at,
           quality_score,
           rejected,
           rejected_at,
           rejected_reason,
           import_confirmed_at,
           episode_key
        FROM watch_matches WHERE NOT hidden ORDER BY created_at DESC LIMIT $1"#,
        limit
    )
    .fetch_all(exec)
    .await?;
    Ok(rows)
}

/// List watch matches by pattern ID.
pub async fn list_matches_by_pattern<'e, E>(
    exec: E,
    pattern_id: Uuid,
    limit: i64,
) -> Result<Vec<WatchMatchRow>>
where
    E: Executor<'e, Database = Postgres>,
{
    let rows = sqlx::query_as!(
        WatchMatchRow,
        r#"SELECT id,
           watch_pattern_id as "watch_pattern_id!",
           indexer_source_id,
           release_guid,
           release_title,
           release_data as "release_data?: serde_json::Value",
           match_score,
           matched_pattern as "matched_pattern!",
           action,
           download_id,
           callback_sent_at,
           callback_error,
           created_at as "created_at!",
           hold_until,
           held,
           callback_attempts,
           callback_last_attempt_at,
           quality_score,
           rejected,
           rejected_at,
           rejected_reason,
           import_confirmed_at,
           episode_key
        FROM watch_matches WHERE watch_pattern_id = $1 AND NOT hidden ORDER BY created_at DESC LIMIT $2"#,
        pattern_id,
        limit
    )
    .fetch_all(exec)
    .await?;
    Ok(rows)
}

/// Check if a release has already been matched for a pattern.
pub async fn already_matched<'e, E>(exec: E, pattern_id: Uuid, release_guid: &str) -> Result<bool>
where
    E: Executor<'e, Database = Postgres>,
{
    let exists = sqlx::query_scalar!(
        r#"SELECT EXISTS(SELECT 1 FROM watch_matches WHERE watch_pattern_id = $1 AND release_guid = $2) as "exists!""#,
        pattern_id,
        release_guid
    )
    .fetch_one(exec)
    .await?;
    Ok(exists)
}

/// Batch-load all matched release GUIDs for a pattern.
///
/// Returns a HashSet for O(1) lookups. Used by the batch scoring pipeline
/// to replace per-release `already_matched()` DB calls with in-memory checks.
pub async fn matched_guids_for_pattern<'e, E>(
    exec: E,
    pattern_id: Uuid,
) -> Result<std::collections::HashSet<String>>
where
    E: Executor<'e, Database = Postgres>,
{
    let guids: Vec<String> = sqlx::query_scalar!(
        "SELECT release_guid FROM watch_matches WHERE watch_pattern_id = $1",
        pattern_id,
    )
    .fetch_all(exec)
    .await?;
    Ok(guids.into_iter().collect())
}

/// Create a watch match record.
///
/// Returns the generated ID. Note: due to ON CONFLICT DO NOTHING, the insert
/// may silently succeed without inserting if a conflict occurs. Use
/// `create_match_if_not_exists` for atomic check-and-create.
pub async fn create_match<'e, E>(exec: E, input: &WatchMatchInput<'_>) -> Result<Uuid>
where
    E: Executor<'e, Database = Postgres>,
{
    let id = Uuid::now_v7();
    sqlx::query!(
        r#"
        INSERT INTO watch_matches
            (id, watch_pattern_id, indexer_source_id, release_guid, release_title, release_data,
             match_score, matched_pattern, action, download_id, hold_until, held,
             quality_score, rejected, rejected_at, rejected_reason, episode_key)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17)
        ON CONFLICT (watch_pattern_id, release_guid) DO NOTHING
        "#,
        id,
        input.pattern_id,
        input.indexer_source_id as Option<Uuid>,
        input.release_guid,
        input.release_title,
        input.release_data as Option<&serde_json::Value>,
        input.match_score,
        input.matched_pattern,
        input.action,
        input.download_id as Option<Uuid>,
        input.hold_until as Option<OffsetDateTime>,
        input.held,
        input.quality_score as Option<f64>,
        input.rejected,
        input.rejected_at as Option<OffsetDateTime>,
        input.rejected_reason,
        input.episode_key,
    )
    .execute(exec)
    .await?;
    Ok(id)
}

/// Atomically create a watch match record only if it doesn't exist.
///
/// Returns `Some(id)` if the insert succeeded (we won the race),
/// or `None` if a match already exists (another thread beat us).
/// This prevents duplicate downloads when two threads process the same release.
pub async fn create_match_if_not_exists<'e, E>(
    exec: E,
    input: &WatchMatchInput<'_>,
) -> Result<Option<Uuid>>
where
    E: Executor<'e, Database = Postgres>,
{
    let id = Uuid::now_v7();
    let result: Option<Uuid> = sqlx::query_scalar!(
        r#"
        INSERT INTO watch_matches
            (id, watch_pattern_id, indexer_source_id, release_guid, release_title, release_data,
             match_score, matched_pattern, action, download_id, hold_until, held,
             quality_score, rejected, rejected_at, rejected_reason, episode_key)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17)
        ON CONFLICT (watch_pattern_id, release_guid) DO NOTHING
        RETURNING id
        "#,
        id,
        input.pattern_id,
        input.indexer_source_id as Option<Uuid>,
        input.release_guid,
        input.release_title,
        input.release_data as Option<&serde_json::Value>,
        input.match_score,
        input.matched_pattern,
        input.action,
        input.download_id as Option<Uuid>,
        input.hold_until as Option<OffsetDateTime>,
        input.held,
        input.quality_score as Option<f64>,
        input.rejected,
        input.rejected_at as Option<OffsetDateTime>,
        input.rejected_reason,
        input.episode_key,
    )
    .fetch_optional(exec)
    .await?;
    Ok(result)
}

/// Reserve a match row, taking over one an earlier automatic pass parked as
/// `skipped`.
///
/// `action = 'skipped'` means "we looked at this release and decided not to grab
/// it" — exactly the decision a person overrides when they ask for another copy.
/// Rows carrying a download or a hold are live state, and `action = 'rejected'`
/// is a person's own blocklisting, so the conflict clause leaves all three alone
/// and this returns `None` just as `create_match_if_not_exists` would.
///
/// `update_match_skipped` also sets `rejected`, so that column cannot tell the
/// two apart; `action` is the only thing that can.
pub async fn reclaim_skipped_match<'e, E>(
    exec: E,
    input: &WatchMatchInput<'_>,
) -> Result<Option<Uuid>>
where
    E: Executor<'e, Database = Postgres>,
{
    let id = Uuid::now_v7();
    let result: Option<Uuid> = sqlx::query_scalar!(
        r#"
        INSERT INTO watch_matches
            (id, watch_pattern_id, indexer_source_id, release_guid, release_title, release_data,
             match_score, matched_pattern, action, download_id, hold_until, held,
             quality_score, rejected, rejected_at, rejected_reason, episode_key)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17)
        ON CONFLICT (watch_pattern_id, release_guid) DO UPDATE
        SET indexer_source_id = EXCLUDED.indexer_source_id,
            release_title = EXCLUDED.release_title,
            release_data = EXCLUDED.release_data,
            match_score = EXCLUDED.match_score,
            matched_pattern = EXCLUDED.matched_pattern,
            action = EXCLUDED.action,
            download_id = EXCLUDED.download_id,
            hold_until = EXCLUDED.hold_until,
            held = EXCLUDED.held,
            quality_score = EXCLUDED.quality_score,
            rejected = EXCLUDED.rejected,
            rejected_at = EXCLUDED.rejected_at,
            rejected_reason = EXCLUDED.rejected_reason,
            episode_key = EXCLUDED.episode_key,
            created_at = NOW()
        WHERE watch_matches.action = 'skipped'
          AND watch_matches.download_id IS NULL
          AND NOT watch_matches.held
        RETURNING id
        "#,
        id,
        input.pattern_id,
        input.indexer_source_id as Option<Uuid>,
        input.release_guid,
        input.release_title,
        input.release_data as Option<&serde_json::Value>,
        input.match_score,
        input.matched_pattern,
        input.action,
        input.download_id as Option<Uuid>,
        input.hold_until as Option<OffsetDateTime>,
        input.held,
        input.quality_score as Option<f64>,
        input.rejected,
        input.rejected_at as Option<OffsetDateTime>,
        input.rejected_reason,
        input.episode_key,
    )
    .fetch_optional(exec)
    .await?;
    Ok(result)
}

/// Update a match record with download_id after download creation.
pub async fn update_match_download_id<'e, E>(exec: E, id: Uuid, download_id: Uuid) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query!(
        "UPDATE watch_matches SET download_id = $2 WHERE id = $1",
        id,
        download_id,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Mark a reserved match as skipped (no download was created).
///
/// Used when `reserve_watch_match` claimed the row but a downstream check
/// (e.g. cutoff already met, create_download error) decided not to grab.
/// Distinct from `reject_match`: there's no download to cancel and no
/// `upgrade_until` to clear — this is a passive "we saw it, decided not to act."
pub async fn update_match_skipped<'e, E>(exec: E, id: Uuid, reason: &str) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query!(
        r#"UPDATE watch_matches
           SET action = 'skipped',
               rejected = TRUE,
               rejected_at = NOW(),
               rejected_reason = $2
           WHERE id = $1"#,
        id,
        reason,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Mark match callback as sent.
pub async fn mark_match_callback_sent<'e, E>(exec: E, id: Uuid) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query!(
        "UPDATE watch_matches SET callback_sent_at = NOW() WHERE id = $1",
        id,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Get held matches that have expired (ready to grab).
pub async fn get_expired_holds<'e, E>(exec: E) -> Result<Vec<WatchMatchRow>>
where
    E: Executor<'e, Database = Postgres>,
{
    let rows = sqlx::query_as!(
        WatchMatchRow,
        r#"SELECT id,
           watch_pattern_id as "watch_pattern_id!",
           indexer_source_id,
           release_guid,
           release_title,
           release_data as "release_data?: serde_json::Value",
           match_score,
           matched_pattern as "matched_pattern!",
           action,
           download_id,
           callback_sent_at,
           callback_error,
           created_at as "created_at!",
           hold_until,
           held,
           callback_attempts,
           callback_last_attempt_at,
           quality_score,
           rejected,
           rejected_at,
           rejected_reason,
           import_confirmed_at,
           episode_key
        FROM watch_matches WHERE held = TRUE AND hold_until <= NOW() ORDER BY created_at"#
    )
    .fetch_all(exec)
    .await?;
    Ok(rows)
}

/// Release a held match: mark as not held and set action/download_id.
pub async fn release_hold<'e, E>(
    exec: E,
    id: Uuid,
    action: &str,
    download_id: Option<Uuid>,
) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query!(
        "UPDATE watch_matches SET held = FALSE, action = $2, download_id = $3 WHERE id = $1",
        id,
        action,
        download_id as Option<Uuid>,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Discard a held match (superseded by a better protocol match).
pub async fn discard_hold<'e, E>(exec: E, id: Uuid) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query!(
        "UPDATE watch_matches SET held = FALSE, action = 'superseded' WHERE id = $1",
        id,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Cancel all held matches for a pattern (and optionally a specific episode_key),
/// except for a specific match. Used when a download is created to clean up stale holds.
pub async fn cancel_held_matches_for_pattern<'e, E>(
    exec: E,
    pattern_id: Uuid,
    episode_key: Option<&str>,
    exclude_match_id: Uuid,
) -> Result<u64>
where
    E: Executor<'e, Database = Postgres>,
{
    let result = sqlx::query(
        r#"UPDATE watch_matches
           SET held = FALSE, action = 'skipped_active_download'
           WHERE watch_pattern_id = $1
             AND held = TRUE
             AND id != $3
             AND ($2::text IS NULL OR episode_key IS NULL OR episode_key = $2)"#,
    )
    .bind(pattern_id)
    .bind(episode_key)
    .bind(exclude_match_id)
    .execute(exec)
    .await?;
    Ok(result.rows_affected())
}

/// Find held matches for a given pattern (to check for supersession).
pub async fn get_held_matches_for_pattern<'e, E>(
    exec: E,
    pattern_id: Uuid,
) -> Result<Vec<WatchMatchRow>>
where
    E: Executor<'e, Database = Postgres>,
{
    let rows = sqlx::query_as!(
        WatchMatchRow,
        r#"SELECT id,
           watch_pattern_id as "watch_pattern_id!",
           indexer_source_id,
           release_guid,
           release_title,
           release_data as "release_data?: serde_json::Value",
           match_score,
           matched_pattern as "matched_pattern!",
           action,
           download_id,
           callback_sent_at,
           callback_error,
           created_at as "created_at!",
           hold_until,
           held,
           callback_attempts,
           callback_last_attempt_at,
           quality_score,
           rejected,
           rejected_at,
           rejected_reason,
           import_confirmed_at,
           episode_key
        FROM watch_matches WHERE watch_pattern_id = $1 AND held = TRUE"#,
        pattern_id
    )
    .fetch_all(exec)
    .await?;
    Ok(rows)
}

/// Update match callback error and increment attempt count.
pub async fn update_match_callback_error<'e, E>(exec: E, id: Uuid, error: &str) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query!(
        r#"UPDATE watch_matches SET
            callback_error = $1,
            callback_attempts = callback_attempts + 1,
            callback_last_attempt_at = NOW()
        WHERE id = $2"#,
        error,
        id,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Get matches needing callback retry (failed but not exhausted).
/// Uses same exponential backoff schedule as download callbacks.
pub async fn get_matches_pending_callback<'e, E>(exec: E) -> Result<Vec<WatchMatchRow>>
where
    E: Executor<'e, Database = Postgres>,
{
    let rows = sqlx::query_as!(
        WatchMatchRow,
        r#"
        SELECT id,
           watch_pattern_id as "watch_pattern_id!",
           indexer_source_id,
           release_guid,
           release_title,
           release_data as "release_data?: serde_json::Value",
           match_score,
           matched_pattern as "matched_pattern!",
           action,
           download_id,
           callback_sent_at,
           callback_error,
           created_at as "created_at!",
           hold_until,
           held,
           callback_attempts,
           callback_last_attempt_at,
           quality_score,
           rejected,
           rejected_at,
           rejected_reason,
           import_confirmed_at,
           episode_key
        FROM watch_matches
        WHERE callback_sent_at IS NULL
          AND callback_error IS NOT NULL
          AND callback_attempts < 10
          AND (
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
          )
        ORDER BY callback_last_attempt_at NULLS FIRST
        LIMIT 10
        "#
    )
    .fetch_all(exec)
    .await?;
    Ok(rows)
}

/// Check if a pattern has an active (non-failed) download.
///
/// This allows retrying with different releases if downloads fail.
/// Only blocks if there's a download that hasn't failed or been cancelled,
/// and hasn't been confirmed as imported yet.
/// Uses LEFT JOIN so it still works if the download row has been deleted
/// (download_id is non-null but the row is gone → no active download).
pub async fn pattern_has_active_download<'e, E>(exec: E, pattern_id: Uuid) -> Result<bool>
where
    E: Executor<'e, Database = Postgres>,
{
    let exists = sqlx::query_scalar!(
        r#"
        SELECT EXISTS(
            SELECT 1 FROM watch_matches wm
            JOIN downloads d ON d.id = wm.download_id
            WHERE wm.watch_pattern_id = $1
              AND wm.download_id IS NOT NULL
              AND d.status NOT IN ('failed', 'cancelled')
              AND wm.import_confirmed_at IS NULL
              AND wm.rejected = FALSE
        ) as "exists!"
        "#,
        pattern_id,
    )
    .fetch_one(exec)
    .await?;
    Ok(exists)
}

/// Episode keys for all active (non-failed/cancelled, not yet imported, not rejected)
/// matches on a pattern. Used by the want fast-path to decide whether every
/// requested episode is already covered before skipping a fresh search.
pub async fn active_episode_keys<'e, E>(exec: E, pattern_id: Uuid) -> Result<Vec<String>>
where
    E: Executor<'e, Database = Postgres>,
{
    let rows = sqlx::query!(
        r#"
        SELECT wm.episode_key as "episode_key!"
        FROM watch_matches wm
        JOIN downloads d ON d.id = wm.download_id
        WHERE wm.watch_pattern_id = $1
          AND wm.episode_key IS NOT NULL
          AND wm.download_id IS NOT NULL
          AND d.status NOT IN ('failed', 'cancelled')
          AND wm.import_confirmed_at IS NULL
          AND wm.rejected = FALSE
        "#,
        pattern_id,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows.into_iter().map(|r| r.episode_key).collect())
}

/// Check if a pattern has any imported (completed) download, regardless of quality_score.
///
/// Uses `watch_matches.import_confirmed_at` directly so that import state
/// survives download deletion (ON DELETE SET NULL on download_id).
///
/// The pattern-level counterpart of the guard in
/// [`episode_has_imported_download`]: a match whose download ended up
/// `failed`/`cancelled` is not a real import, so it must not mark a movie (or
/// any non-season pattern) as satisfied. LEFT JOIN so imports whose download row
/// was since deleted still count.
pub async fn pattern_has_imported_download<'e, E>(exec: E, pattern_id: Uuid) -> Result<bool>
where
    E: Executor<'e, Database = Postgres>,
{
    let exists = sqlx::query_scalar!(
        r#"
        SELECT EXISTS(
            SELECT 1 FROM watch_matches wm
            LEFT JOIN downloads d ON d.id = wm.download_id
            WHERE wm.watch_pattern_id = $1
              AND wm.import_confirmed_at IS NOT NULL
              AND wm.rejected = FALSE
              AND (d.id IS NULL OR d.status NOT IN ('failed', 'cancelled'))
        ) as "exists!"
        "#,
        pattern_id,
    )
    .fetch_one(exec)
    .await?;
    Ok(exists)
}

/// Check if a pattern has any currently held match.
pub async fn pattern_has_held_match<'e, E>(exec: E, pattern_id: Uuid) -> Result<bool>
where
    E: Executor<'e, Database = Postgres>,
{
    let exists = sqlx::query_scalar!(
        r#"
        SELECT EXISTS(
            SELECT 1 FROM watch_matches
            WHERE watch_pattern_id = $1
              AND held = TRUE
        ) as "exists!"
        "#,
        pattern_id,
    )
    .fetch_one(exec)
    .await?;
    Ok(exists)
}

/// Get the best (highest) quality score from imported downloads for a pattern.
///
/// Uses `watch_matches.import_confirmed_at` directly — no downloads JOIN needed.
pub async fn get_best_imported_quality_for_pattern<'e, E>(
    exec: E,
    pattern_id: Uuid,
) -> Result<Option<f64>>
where
    E: Executor<'e, Database = Postgres>,
{
    let score = sqlx::query_scalar!(
        r#"SELECT MAX(quality_score) as "score?"
        FROM watch_matches
        WHERE watch_pattern_id = $1
          AND import_confirmed_at IS NOT NULL
          AND quality_score IS NOT NULL
          AND rejected = FALSE"#,
        pattern_id
    )
    .fetch_one(exec)
    .await?;
    Ok(score)
}

/// Get the download_id of the most recently imported download for a pattern.
///
/// Uses `watch_matches.import_confirmed_at` directly. Note: download_id may be NULL
/// if the download was deleted, but we still return it for informational purposes.
pub async fn get_latest_imported_download_for_pattern<'e, E>(
    exec: E,
    pattern_id: Uuid,
) -> Result<Option<Uuid>>
where
    E: Executor<'e, Database = Postgres>,
{
    let download_id = sqlx::query_scalar!(
        r#"SELECT download_id as "download_id?"
        FROM watch_matches
        WHERE watch_pattern_id = $1
          AND import_confirmed_at IS NOT NULL
          AND rejected = FALSE
        ORDER BY import_confirmed_at DESC
        LIMIT 1"#,
        pattern_id
    )
    .fetch_optional(exec)
    .await?;
    Ok(download_id.flatten())
}

/// Reject a watch match — marks it rejected and cancels the associated download.
/// If the rejected match was the last confirmed import for its pattern, clears
/// `upgrade_until` so the pattern returns to "searching" status.
///
/// Returns true if the match existed (regardless of whether the download was cancelled).
pub async fn reject_match(
    pool: &sqlx::PgPool,
    match_id: Uuid,
    reason: Option<&str>,
) -> Result<bool> {
    let found = sqlx::query!(
        r#"
        UPDATE watch_matches
        SET rejected = TRUE, rejected_at = NOW(), rejected_reason = $2, action = 'rejected'
        WHERE id = $1
        RETURNING download_id, watch_pattern_id as "watch_pattern_id!"
        "#,
        match_id,
        reason,
    )
    .fetch_optional(pool)
    .await?;

    let Some(row) = found else {
        return Ok(false);
    };

    if let Some(dl_id) = row.download_id {
        sqlx::query!(
            r#"UPDATE downloads SET status = 'cancelled'
            WHERE id = $1
              AND status NOT IN ('completed', 'cancelled', 'failed')"#,
            dl_id,
        )
        .execute(pool)
        .await?;
    }

    // If no non-rejected confirmed imports remain, clear upgrade_until
    if !pattern_has_imported_download(pool, row.watch_pattern_id).await? {
        super::patterns::clear_upgrade_until(pool, row.watch_pattern_id).await?;
    }

    Ok(true)
}

/// Find the watch pattern ID associated with a download.
pub async fn get_watch_pattern_for_download<'e, E>(
    exec: E,
    download_id: Uuid,
) -> Result<Option<Uuid>>
where
    E: Executor<'e, Database = Postgres>,
{
    let row = sqlx::query_scalar!(
        "SELECT watch_pattern_id FROM watch_matches WHERE download_id = $1 LIMIT 1",
        download_id,
    )
    .fetch_optional(exec)
    .await?;
    Ok(row.flatten())
}

/// Get the watch_pattern_id and release_title for a given download.
/// Used when a download fails to exclude the release and re-search.
pub async fn get_match_info_for_download<'e, E>(
    exec: E,
    download_id: Uuid,
) -> Result<Option<(Uuid, String)>>
where
    E: Executor<'e, Database = Postgres>,
{
    let row = sqlx::query!(
        r#"
        SELECT watch_pattern_id, release_title
        FROM watch_matches
        WHERE download_id = $1
        LIMIT 1
        "#,
        download_id,
    )
    .fetch_optional(exec)
    .await?;
    Ok(row.and_then(|r| r.watch_pattern_id.map(|pid| (pid, r.release_title))))
}

/// Mark watch matches as import-confirmed for a given download.
///
/// Called when `confirm_import` succeeds on the downloads table.
/// This preserves import state on the watch_match row so it survives
/// download deletion (ON DELETE SET NULL on download_id).
///
/// Joins `downloads` so a `failed`/`cancelled` download can never be marked
/// imported — the stamp gates per-episode re-search, so a late confirmation
/// racing a content-validation failure would otherwise strand the episode.
pub async fn mark_match_import_confirmed<'e, E>(exec: E, download_id: Uuid) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query!(
        r#"UPDATE watch_matches wm
        SET import_confirmed_at = NOW()
        FROM downloads d
        WHERE wm.download_id = $1
          AND d.id = wm.download_id
          AND wm.import_confirmed_at IS NULL
          AND d.status NOT IN ('failed', 'cancelled')"#,
        download_id,
    )
    .execute(exec)
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Per-episode queries for TvSeason patterns
// ---------------------------------------------------------------------------

/// Check if a specific episode has an active (non-failed, non-imported) download.
///
/// Also returns true if a SEASON_PACK is active (covers all episodes).
pub async fn episode_has_active_download(
    pool: &sqlx::PgPool,
    pattern_id: Uuid,
    episode_key: &str,
) -> Result<bool> {
    let exists = sqlx::query_scalar!(
        r#"
        SELECT EXISTS(
            SELECT 1 FROM watch_matches wm
            JOIN downloads d ON d.id = wm.download_id
            WHERE wm.watch_pattern_id = $1
              AND (wm.episode_key = $2 OR wm.episode_key = 'SEASON_PACK')
              AND wm.download_id IS NOT NULL
              AND d.status NOT IN ('failed', 'cancelled')
              AND wm.import_confirmed_at IS NULL
              AND wm.rejected = FALSE
        ) as "exists!"
        "#,
        pattern_id,
        episode_key,
    )
    .fetch_one(pool)
    .await?;
    Ok(exists)
}

/// Check if a specific episode has been imported.
///
/// Also returns true if a SEASON_PACK has been imported.
///
/// A match whose download later ended up `failed`/`cancelled` does not count as
/// imported. The write side prevents new bad stamps, but this LEFT JOIN also
/// neutralises rows stamped before that guard existed, so a stranded episode
/// starts searching again on its own rather than needing a manual repair. The
/// join is LEFT because `download_id` is nulled when a download is deleted, and
/// those older imports must still count.
pub async fn episode_has_imported_download(
    pool: &sqlx::PgPool,
    pattern_id: Uuid,
    episode_key: &str,
) -> Result<bool> {
    let exists = sqlx::query_scalar!(
        r#"
        SELECT EXISTS(
            SELECT 1 FROM watch_matches wm
            LEFT JOIN downloads d ON d.id = wm.download_id
            WHERE wm.watch_pattern_id = $1
              AND (wm.episode_key = $2 OR wm.episode_key = 'SEASON_PACK')
              AND wm.import_confirmed_at IS NOT NULL
              AND wm.rejected = FALSE
              AND (d.id IS NULL OR d.status NOT IN ('failed', 'cancelled'))
        ) as "exists!"
        "#,
        pattern_id,
        episode_key,
    )
    .fetch_one(pool)
    .await?;
    Ok(exists)
}

/// Best imported quality score for a specific episode.
pub async fn get_best_imported_quality_for_episode(
    pool: &sqlx::PgPool,
    pattern_id: Uuid,
    episode_key: &str,
) -> Result<Option<f64>> {
    let score = sqlx::query_scalar!(
        r#"SELECT MAX(quality_score) as "score?"
        FROM watch_matches
        WHERE watch_pattern_id = $1
          AND (episode_key = $2 OR episode_key = 'SEASON_PACK')
          AND import_confirmed_at IS NOT NULL
          AND quality_score IS NOT NULL
          AND rejected = FALSE"#,
        pattern_id,
        episode_key,
    )
    .fetch_one(pool)
    .await?;
    Ok(score)
}

/// Release title of the highest-scoring imported match for a specific episode.
///
/// Used by the upgrade gate to compare the existing release's resolution tier
/// against a candidate — a higher composite quality_score must NOT let a lower
/// resolution (e.g. 720p WEB-DL) replace a higher one (1080p WEB).
pub async fn get_best_imported_title_for_episode(
    pool: &sqlx::PgPool,
    pattern_id: Uuid,
    episode_key: &str,
) -> Result<Option<String>> {
    let title = sqlx::query_scalar!(
        r#"SELECT release_title
        FROM watch_matches
        WHERE watch_pattern_id = $1
          AND (episode_key = $2 OR episode_key = 'SEASON_PACK')
          AND import_confirmed_at IS NOT NULL
          AND quality_score IS NOT NULL
          AND rejected = FALSE
        ORDER BY quality_score DESC
        LIMIT 1"#,
        pattern_id,
        episode_key,
    )
    .fetch_optional(pool)
    .await?;
    Ok(title)
}

/// Release title of the highest-scoring imported match for a pattern.
/// Pattern-level companion to [`get_best_imported_title_for_episode`].
pub async fn get_best_imported_title_for_pattern(
    pool: &sqlx::PgPool,
    pattern_id: Uuid,
) -> Result<Option<String>> {
    let title = sqlx::query_scalar!(
        r#"SELECT release_title
        FROM watch_matches
        WHERE watch_pattern_id = $1
          AND import_confirmed_at IS NOT NULL
          AND quality_score IS NOT NULL
          AND rejected = FALSE
        ORDER BY quality_score DESC
        LIMIT 1"#,
        pattern_id,
    )
    .fetch_optional(pool)
    .await?;
    Ok(title)
}

/// Latest imported download_id for a specific episode (for upgrade replacement).
pub async fn get_latest_imported_download_for_episode(
    pool: &sqlx::PgPool,
    pattern_id: Uuid,
    episode_key: &str,
) -> Result<Option<Uuid>> {
    let download_id = sqlx::query_scalar!(
        r#"SELECT download_id as "download_id?"
        FROM watch_matches
        WHERE watch_pattern_id = $1
          AND (episode_key = $2 OR episode_key = 'SEASON_PACK')
          AND import_confirmed_at IS NOT NULL
          AND rejected = FALSE
        ORDER BY import_confirmed_at DESC
        LIMIT 1"#,
        pattern_id,
        episode_key,
    )
    .fetch_optional(pool)
    .await?;
    Ok(download_id.flatten())
}

/// All distinct episode_keys with confirmed imports for a pattern.
pub async fn get_imported_episode_keys(
    pool: &sqlx::PgPool,
    pattern_id: Uuid,
) -> Result<Vec<String>> {
    let rows = sqlx::query_scalar!(
        r#"SELECT DISTINCT episode_key as "episode_key!"
        FROM watch_matches
        WHERE watch_pattern_id = $1
          AND episode_key IS NOT NULL
          AND import_confirmed_at IS NOT NULL
          AND rejected = FALSE"#,
        pattern_id,
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

// =============================================================================
// Bulk Operations
// =============================================================================

/// Bulk reject watch matches by IDs. Returns the number of rows updated.
/// Clears `upgrade_until` on any affected patterns that no longer have
/// non-rejected confirmed imports.
pub async fn bulk_reject_matches(
    pool: &sqlx::PgPool,
    ids: &[Uuid],
    reason: Option<&str>,
) -> Result<u64> {
    // Reject and collect distinct affected pattern IDs
    let affected = sqlx::query_scalar!(
        r#"UPDATE watch_matches
        SET rejected = TRUE, rejected_at = NOW(), rejected_reason = $2
        WHERE id = ANY($1)
        RETURNING watch_pattern_id as "watch_pattern_id!""#,
        ids,
        reason,
    )
    .fetch_all(pool)
    .await?;

    let count = affected.len() as u64;

    // Deduplicate pattern IDs and check each
    let mut seen = std::collections::HashSet::new();
    for pattern_id in affected {
        if seen.insert(pattern_id) && !pattern_has_imported_download(pool, pattern_id).await? {
            super::patterns::clear_upgrade_until(pool, pattern_id).await?;
        }
    }

    Ok(count)
}

/// Soft-delete watch matches by IDs (hide from UI, keep for watch system).
pub async fn bulk_delete_matches<'e, E>(exec: E, ids: &[Uuid]) -> Result<u64>
where
    E: Executor<'e, Database = Postgres>,
{
    let result = sqlx::query!(
        "UPDATE watch_matches SET hidden = TRUE WHERE id = ANY($1) AND NOT hidden",
        ids,
    )
    .execute(exec)
    .await?;
    Ok(result.rows_affected())
}

/// Per-pattern download state for batch status computation.
pub struct PatternDownloadState {
    pub pattern_id: Uuid,
    pub has_active: bool,
    pub has_imported: bool,
    pub has_held: bool,
}

/// Batch-fetch download state for all patterns in a single query.
pub async fn batch_pattern_download_state<'e, E>(exec: E) -> Result<Vec<PatternDownloadState>>
where
    E: Executor<'e, Database = Postgres>,
{
    let rows = sqlx::query!(
        r#"
        SELECT
            wp.id as pattern_id,
            EXISTS(
                SELECT 1 FROM watch_matches wm
                JOIN downloads d ON d.id = wm.download_id
                WHERE wm.watch_pattern_id = wp.id
                  AND wm.download_id IS NOT NULL
                  AND d.status NOT IN ('failed', 'cancelled')
                  AND wm.import_confirmed_at IS NULL
                  AND wm.rejected = FALSE
            ) as "has_active!",
            EXISTS(
                SELECT 1 FROM watch_matches
                WHERE watch_pattern_id = wp.id
                  AND import_confirmed_at IS NOT NULL
                  AND rejected = FALSE
            ) as "has_imported!",
            EXISTS(
                SELECT 1 FROM watch_matches
                WHERE watch_pattern_id = wp.id
                  AND held = TRUE
            ) as "has_held!"
        FROM watch_patterns wp
        "#,
    )
    .fetch_all(exec)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| PatternDownloadState {
            pattern_id: r.pattern_id,
            has_active: r.has_active,
            has_imported: r.has_imported,
            has_held: r.has_held,
        })
        .collect())
}
