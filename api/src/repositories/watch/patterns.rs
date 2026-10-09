//! Watch patterns: CRUD and sync operations.

use super::super::Result;
use sqlx::{Executor, Postgres};
use time::OffsetDateTime;
use uuid::Uuid;

// =============================================================================
// Cursor Pagination
// =============================================================================

/// Pagination options for watch pattern list.
#[derive(Debug, Default)]
pub struct WatchPatternListOptions {
    pub limit: i64,
    pub cursor: Option<OffsetDateTime>,
    pub cursor_id: Option<Uuid>,
    pub source_system: Option<String>,
    pub enabled: Option<bool>,
    pub media_type_id: Option<Uuid>,
    pub search: Option<String>,
}

/// Response for paginated watch pattern list.
#[derive(Debug)]
pub struct WatchPatternListResponse {
    pub items: Vec<WatchPatternRow>,
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

/// Fetch watch pattern list with cursor pagination and optional filters.
pub async fn fetch_pattern_list<'e, E>(
    exec: E,
    options: WatchPatternListOptions,
) -> Result<WatchPatternListResponse>
where
    E: Executor<'e, Database = Postgres>,
{
    let limit = options.limit.clamp(1, 100);
    let fetch_limit = limit + 1;

    let items = sqlx::query_as!(
        WatchPatternRow,
        r#"SELECT id, source_system, source_id, callback_url, patterns,
               media_want, match_targets, match_mode as "match_mode?",
               min_score, media_type_id, auto_download as "auto_download!",
               metadata, enabled as "enabled!",
               upgrade_allowed as "upgrade_allowed!", upgrade_until,
               created_at as "created_at!", updated_at as "updated_at!",
               last_backlog_search_at, backlog_search_interval_hours,
               excluded_releases, search_not_before
        FROM watch_patterns
        WHERE ($1::timestamptz IS NULL OR (created_at, id) < ($1, $2))
          AND ($3::text IS NULL OR source_system = $3)
          AND ($4::bool IS NULL OR enabled = $4)
          AND ($5::uuid IS NULL OR media_type_id = $5)
          AND ($7::text IS NULL OR source_id ILIKE '%' || $7 || '%'
               OR array_to_string(patterns, ' ') ILIKE '%' || $7 || '%'
               OR media_want::text ILIKE '%' || $7 || '%')
        ORDER BY created_at DESC, id DESC
        LIMIT $6"#,
        options.cursor as Option<OffsetDateTime>,
        options.cursor_id as Option<Uuid>,
        options.source_system.as_deref() as Option<&str>,
        options.enabled as Option<bool>,
        options.media_type_id as Option<Uuid>,
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

    Ok(WatchPatternListResponse { items, next_cursor })
}

/// Watch pattern row from database.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct WatchPatternRow {
    pub id: Uuid,
    pub source_system: String,
    pub source_id: String,
    pub callback_url: String,
    pub patterns: Vec<String>,
    pub media_want: Option<serde_json::Value>,
    pub match_targets: Option<serde_json::Value>,
    pub match_mode: Option<String>,
    pub min_score: Option<f64>,
    pub media_type_id: Option<Uuid>,
    pub auto_download: bool,
    pub metadata: Option<serde_json::Value>,
    pub enabled: bool,
    pub upgrade_allowed: bool,
    pub upgrade_until: Option<OffsetDateTime>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
    pub last_backlog_search_at: Option<OffsetDateTime>,
    pub backlog_search_interval_hours: Option<i32>,
    pub excluded_releases: Option<serde_json::Value>,
    pub search_not_before: Option<OffsetDateTime>,
}

/// Input for creating/updating a watch pattern.
#[derive(Debug)]
pub struct WatchPatternInput {
    pub callback_url: String,
    pub patterns: Vec<String>,
    pub media_want: Option<serde_json::Value>,
    pub match_targets: Option<serde_json::Value>,
    pub match_mode: Option<String>,
    pub min_score: Option<f64>,
    pub media_type_id: Option<Uuid>,
    pub auto_download: bool,
    pub metadata: Option<serde_json::Value>,
    pub enabled: bool,
    pub upgrade_allowed: bool,
    pub search_not_before: Option<OffsetDateTime>,
}

/// Input for Kobload-owned updates to a watch pattern.
#[derive(Debug, Default)]
pub struct WatchPatternUpdate {
    pub patterns: Option<Vec<String>>,
    pub media_want: Option<Option<serde_json::Value>>,
    pub match_mode: Option<String>,
    pub min_score: Option<f64>,
    pub enabled: Option<bool>,
}

/// List all watch patterns.
pub async fn list_patterns<'e, E>(exec: E) -> Result<Vec<WatchPatternRow>>
where
    E: Executor<'e, Database = Postgres>,
{
    let rows = sqlx::query_as!(
        WatchPatternRow,
        r#"SELECT id, source_system, source_id, callback_url, patterns,
               media_want, match_targets, match_mode as "match_mode?",
               min_score, media_type_id, auto_download as "auto_download!",
               metadata, enabled as "enabled!",
               upgrade_allowed as "upgrade_allowed!", upgrade_until,
               created_at as "created_at!", updated_at as "updated_at!",
               last_backlog_search_at, backlog_search_interval_hours,
               excluded_releases, search_not_before
        FROM watch_patterns ORDER BY created_at DESC"#
    )
    .fetch_all(exec)
    .await?;
    Ok(rows)
}

/// List watch patterns by source system.
pub async fn list_patterns_by_source<'e, E>(
    exec: E,
    source_system: &str,
) -> Result<Vec<WatchPatternRow>>
where
    E: Executor<'e, Database = Postgres>,
{
    let rows = sqlx::query_as!(
        WatchPatternRow,
        r#"SELECT id, source_system, source_id, callback_url, patterns,
               media_want, match_targets, match_mode as "match_mode?",
               min_score, media_type_id, auto_download as "auto_download!",
               metadata, enabled as "enabled!",
               upgrade_allowed as "upgrade_allowed!", upgrade_until,
               created_at as "created_at!", updated_at as "updated_at!",
               last_backlog_search_at, backlog_search_interval_hours,
               excluded_releases, search_not_before
        FROM watch_patterns WHERE source_system = $1 ORDER BY created_at DESC"#,
        source_system
    )
    .fetch_all(exec)
    .await?;
    Ok(rows)
}

/// List watch patterns by source system and enabled status.
pub async fn list_patterns_by_source_enabled<'e, E>(
    exec: E,
    source_system: &str,
    enabled: bool,
) -> Result<Vec<WatchPatternRow>>
where
    E: Executor<'e, Database = Postgres>,
{
    let rows = sqlx::query_as!(
        WatchPatternRow,
        r#"SELECT id, source_system, source_id, callback_url, patterns,
               media_want, match_targets, match_mode as "match_mode?",
               min_score, media_type_id, auto_download as "auto_download!",
               metadata, enabled as "enabled!",
               upgrade_allowed as "upgrade_allowed!", upgrade_until,
               created_at as "created_at!", updated_at as "updated_at!",
               last_backlog_search_at, backlog_search_interval_hours,
               excluded_releases, search_not_before
        FROM watch_patterns WHERE source_system = $1 AND enabled = $2 ORDER BY created_at DESC"#,
        source_system,
        enabled
    )
    .fetch_all(exec)
    .await?;
    Ok(rows)
}

/// List watch patterns by enabled status.
pub async fn list_patterns_by_enabled<'e, E>(exec: E, enabled: bool) -> Result<Vec<WatchPatternRow>>
where
    E: Executor<'e, Database = Postgres>,
{
    let rows = sqlx::query_as!(
        WatchPatternRow,
        r#"SELECT id, source_system, source_id, callback_url, patterns,
               media_want, match_targets, match_mode as "match_mode?",
               min_score, media_type_id, auto_download as "auto_download!",
               metadata, enabled as "enabled!",
               upgrade_allowed as "upgrade_allowed!", upgrade_until,
               created_at as "created_at!", updated_at as "updated_at!",
               last_backlog_search_at, backlog_search_interval_hours,
               excluded_releases, search_not_before
        FROM watch_patterns WHERE enabled = $1 ORDER BY created_at DESC"#,
        enabled
    )
    .fetch_all(exec)
    .await?;
    Ok(rows)
}

/// Get a watch pattern by ID.
pub async fn get_pattern_by_id<'e, E>(exec: E, id: Uuid) -> Result<Option<WatchPatternRow>>
where
    E: Executor<'e, Database = Postgres>,
{
    let row = sqlx::query_as!(
        WatchPatternRow,
        r#"SELECT id, source_system, source_id, callback_url, patterns,
               media_want, match_targets, match_mode as "match_mode?",
               min_score, media_type_id, auto_download as "auto_download!",
               metadata, enabled as "enabled!",
               upgrade_allowed as "upgrade_allowed!", upgrade_until,
               created_at as "created_at!", updated_at as "updated_at!",
               last_backlog_search_at, backlog_search_interval_hours,
               excluded_releases, search_not_before
        FROM watch_patterns WHERE id = $1"#,
        id
    )
    .fetch_optional(exec)
    .await?;
    Ok(row)
}

/// Get a watch pattern by source_system and source_id.
pub async fn get_pattern_by_source<'e, E>(
    exec: E,
    source_system: &str,
    source_id: &str,
) -> Result<Option<WatchPatternRow>>
where
    E: Executor<'e, Database = Postgres>,
{
    let row = sqlx::query_as!(
        WatchPatternRow,
        r#"SELECT id, source_system, source_id, callback_url, patterns,
               media_want, match_targets, match_mode as "match_mode?",
               min_score, media_type_id, auto_download as "auto_download!",
               metadata, enabled as "enabled!",
               upgrade_allowed as "upgrade_allowed!", upgrade_until,
               created_at as "created_at!", updated_at as "updated_at!",
               last_backlog_search_at, backlog_search_interval_hours,
               excluded_releases, search_not_before
        FROM watch_patterns WHERE source_system = $1 AND source_id = $2"#,
        source_system,
        source_id
    )
    .fetch_optional(exec)
    .await?;
    Ok(row)
}

/// Check if a watch pattern exists.
pub async fn pattern_exists<'e, E>(exec: E, source_system: &str, source_id: &str) -> Result<bool>
where
    E: Executor<'e, Database = Postgres>,
{
    let exists = sqlx::query_scalar!(
        r#"SELECT EXISTS(SELECT 1 FROM watch_patterns WHERE source_system = $1 AND source_id = $2) as "exists!""#,
        source_system,
        source_id
    )
    .fetch_one(exec)
    .await?;
    Ok(exists)
}

/// Check if there are any active (enabled) watch patterns.
pub async fn has_active_patterns<'e, E>(exec: E) -> Result<bool>
where
    E: Executor<'e, Database = Postgres>,
{
    let exists = sqlx::query_scalar!(
        r#"SELECT EXISTS(SELECT 1 FROM watch_patterns WHERE enabled = TRUE) as "exists!""#
    )
    .fetch_one(exec)
    .await?;
    Ok(exists)
}

/// Create a watch pattern.
pub async fn create_pattern<'e, E>(
    exec: E,
    source_system: &str,
    source_id: &str,
    input: WatchPatternInput,
) -> Result<Uuid>
where
    E: Executor<'e, Database = Postgres>,
{
    let id = Uuid::now_v7();
    sqlx::query!(
        r#"
        INSERT INTO watch_patterns
            (id, source_system, source_id, callback_url, patterns, media_want, match_targets, match_mode, min_score,
             media_type_id, auto_download, metadata, enabled, upgrade_allowed, search_not_before)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15)
        "#,
        id,
        source_system,
        source_id,
        &input.callback_url,
        &input.patterns,
        input.media_want as Option<serde_json::Value>,
        input.match_targets as Option<serde_json::Value>,
        input.match_mode.as_deref(),
        input.min_score as Option<f64>,
        input.media_type_id as Option<Uuid>,
        input.auto_download,
        input.metadata as Option<serde_json::Value>,
        input.enabled,
        input.upgrade_allowed,
        input.search_not_before as Option<OffsetDateTime>,
    )
    .execute(exec)
    .await?;
    Ok(id)
}

/// Update a watch pattern (full sync from consumer).
pub async fn update_pattern_full<'e, E>(
    exec: E,
    source_system: &str,
    source_id: &str,
    input: WatchPatternInput,
) -> Result<bool>
where
    E: Executor<'e, Database = Postgres>,
{
    let result = sqlx::query!(
        r#"
        UPDATE watch_patterns SET
            callback_url = $3,
            patterns = $4,
            media_want = $5,
            match_targets = $6,
            match_mode = $7,
            min_score = $8,
            media_type_id = $9,
            auto_download = $10,
            metadata = $11,
            enabled = $12,
            upgrade_allowed = $13,
            search_not_before = $14,
            updated_at = NOW()
        WHERE source_system = $1 AND source_id = $2
        "#,
        source_system,
        source_id,
        &input.callback_url,
        &input.patterns,
        input.media_want as Option<serde_json::Value>,
        input.match_targets as Option<serde_json::Value>,
        input.match_mode.as_deref(),
        input.min_score as Option<f64>,
        input.media_type_id as Option<Uuid>,
        input.auto_download,
        input.metadata as Option<serde_json::Value>,
        input.enabled,
        input.upgrade_allowed,
        input.search_not_before as Option<OffsetDateTime>,
    )
    .execute(exec)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Update a watch pattern by ID (full update).
pub async fn update_pattern_by_id<'e, E>(
    exec: E,
    id: Uuid,
    input: WatchPatternInput,
) -> Result<bool>
where
    E: Executor<'e, Database = Postgres>,
{
    let result = sqlx::query!(
        r#"
        UPDATE watch_patterns SET
            callback_url = $2,
            patterns = $3,
            media_want = $4,
            match_targets = $5,
            match_mode = $6,
            min_score = $7,
            media_type_id = $8,
            auto_download = $9,
            metadata = $10,
            enabled = $11,
            upgrade_allowed = $12,
            search_not_before = $13,
            updated_at = NOW()
        WHERE id = $1
        "#,
        id,
        &input.callback_url,
        &input.patterns,
        input.media_want as Option<serde_json::Value>,
        input.match_targets as Option<serde_json::Value>,
        input.match_mode.as_deref(),
        input.min_score as Option<f64>,
        input.media_type_id as Option<Uuid>,
        input.auto_download,
        input.metadata as Option<serde_json::Value>,
        input.enabled,
        input.upgrade_allowed,
        input.search_not_before as Option<OffsetDateTime>,
    )
    .execute(exec)
    .await?;

    Ok(result.rows_affected() > 0)
}

/// Toggle a watch pattern's enabled state.
pub async fn set_pattern_enabled<'e, E>(exec: E, id: Uuid, enabled: bool) -> Result<bool>
where
    E: Executor<'e, Database = Postgres>,
{
    let result = sqlx::query!(
        "UPDATE watch_patterns SET enabled = $2, updated_at = NOW() WHERE id = $1",
        id,
        enabled,
    )
    .execute(exec)
    .await?;

    Ok(result.rows_affected() > 0)
}

/// Delete a watch pattern by source.
pub async fn delete_pattern_by_source<'e, E>(
    exec: E,
    source_system: &str,
    source_id: &str,
) -> Result<bool>
where
    E: Executor<'e, Database = Postgres>,
{
    let result = sqlx::query!(
        "DELETE FROM watch_patterns WHERE source_system = $1 AND source_id = $2",
        source_system,
        source_id,
    )
    .execute(exec)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Get patterns matching a media type ID.
pub async fn get_patterns_for_media_type<'e, E>(
    exec: E,
    media_type_id: Uuid,
) -> Result<Vec<WatchPatternRow>>
where
    E: Executor<'e, Database = Postgres>,
{
    let rows = sqlx::query_as!(
        WatchPatternRow,
        r#"SELECT id, source_system, source_id, callback_url, patterns,
               media_want, match_targets, match_mode as "match_mode?",
               min_score, media_type_id, auto_download as "auto_download!",
               metadata, enabled as "enabled!",
               upgrade_allowed as "upgrade_allowed!", upgrade_until,
               created_at as "created_at!", updated_at as "updated_at!",
               last_backlog_search_at, backlog_search_interval_hours,
               excluded_releases, search_not_before
        FROM watch_patterns
        WHERE enabled = TRUE
          AND (media_type_id = $1 OR media_type_id IS NULL)"#,
        media_type_id
    )
    .fetch_all(exec)
    .await?;
    Ok(rows)
}

/// Get all enabled watch patterns.
pub async fn get_all_enabled_patterns<'e, E>(exec: E) -> Result<Vec<WatchPatternRow>>
where
    E: Executor<'e, Database = Postgres>,
{
    let rows = sqlx::query_as!(
        WatchPatternRow,
        r#"SELECT id, source_system, source_id, callback_url, patterns,
               media_want, match_targets, match_mode as "match_mode?",
               min_score, media_type_id, auto_download as "auto_download!",
               metadata, enabled as "enabled!",
               upgrade_allowed as "upgrade_allowed!", upgrade_until,
               created_at as "created_at!", updated_at as "updated_at!",
               last_backlog_search_at, backlog_search_interval_hours,
               excluded_releases, search_not_before
        FROM watch_patterns WHERE enabled = TRUE"#
    )
    .fetch_all(exec)
    .await?;
    Ok(rows)
}

// =============================================================================
// Watch Pattern Sync Operations
// =============================================================================

/// Input for syncing/upserting a watch pattern from an external source.
#[derive(Debug, Default)]
pub struct WatchPatternSyncInput {
    pub source_system: String,
    pub source_id: String,
    pub callback_url: Option<String>,
    pub patterns: Vec<String>,
    pub media_want: Option<serde_json::Value>,
    pub match_targets: Option<serde_json::Value>,
    pub match_mode: Option<String>,
    pub min_score: Option<f64>,
    pub media_type_id: Option<Uuid>,
    pub auto_download: bool,
    pub metadata: Option<serde_json::Value>,
    pub enabled: bool,
    pub upgrade_allowed: bool,
    pub excluded_releases: Option<serde_json::Value>,
    pub search_not_before: Option<OffsetDateTime>,
}

/// Update pattern by source_system and source_id with all sync fields.
/// Update a pattern from a client sync.
///
/// `excluded_releases` is merged, not replaced. The client only knows what it
/// blocklisted itself; Kobload adds its own entries as downloads fail
/// (`trigger_research_for_download`), and a sync that overwrote the column threw
/// those away — handing the next search the release it just watched fail. The
/// union is deduplicated on the whole entry, and jsonb compares structurally, so
/// re-sending the same blocklist is idempotent rather than cumulative.
pub async fn update_pattern_by_source<'e, E>(exec: E, input: &WatchPatternSyncInput) -> Result<bool>
where
    E: Executor<'e, Database = Postgres>,
{
    let result = sqlx::query!(
        r#"
        UPDATE watch_patterns SET
            callback_url = $3,
            patterns = $4,
            media_want = $5,
            match_targets = $6,
            match_mode = $7,
            min_score = $8,
            media_type_id = $9,
            auto_download = $10,
            metadata = $11,
            enabled = $12,
            upgrade_allowed = $13,
            upgrade_until = NULL,
            excluded_releases = (
                SELECT COALESCE(jsonb_agg(DISTINCT entry), '[]'::jsonb)
                FROM jsonb_array_elements(
                    COALESCE(watch_patterns.excluded_releases, '[]'::jsonb)
                    || COALESCE($14::jsonb, '[]'::jsonb)
                ) AS entry
            ),
            search_not_before = $15,
            updated_at = NOW()
        WHERE source_system = $1 AND source_id = $2
        "#,
        &input.source_system,
        &input.source_id,
        input.callback_url.as_deref(),
        &input.patterns,
        input.media_want.as_ref() as Option<&serde_json::Value>,
        input.match_targets.as_ref() as Option<&serde_json::Value>,
        input.match_mode.as_deref(),
        input.min_score as Option<f64>,
        input.media_type_id as Option<Uuid>,
        input.auto_download,
        input.metadata.as_ref() as Option<&serde_json::Value>,
        input.enabled,
        input.upgrade_allowed,
        input.excluded_releases.as_ref() as Option<&serde_json::Value>,
        input.search_not_before as Option<OffsetDateTime>,
    )
    .execute(exec)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Insert a new watch pattern from sync.
pub async fn insert_pattern_from_sync<'e, E>(exec: E, input: &WatchPatternSyncInput) -> Result<Uuid>
where
    E: Executor<'e, Database = Postgres>,
{
    let id = Uuid::now_v7();
    sqlx::query!(
        r#"
        INSERT INTO watch_patterns
            (id, source_system, source_id, callback_url, patterns, media_want, match_targets, match_mode, min_score,
             media_type_id, auto_download, metadata, enabled, upgrade_allowed, excluded_releases, search_not_before)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16)
        "#,
        id,
        &input.source_system,
        &input.source_id,
        input.callback_url.as_deref(),
        &input.patterns,
        input.media_want.as_ref() as Option<&serde_json::Value>,
        input.match_targets.as_ref() as Option<&serde_json::Value>,
        input.match_mode.as_deref(),
        input.min_score as Option<f64>,
        input.media_type_id as Option<Uuid>,
        input.auto_download,
        input.metadata.as_ref() as Option<&serde_json::Value>,
        input.enabled,
        input.upgrade_allowed,
        input.excluded_releases.as_ref() as Option<&serde_json::Value>,
        input.search_not_before as Option<OffsetDateTime>,
    )
    .execute(exec)
    .await?;
    Ok(id)
}

/// Partial update input for watch pattern.
#[derive(Debug, Default)]
pub struct WatchPatternPartialUpdate {
    pub patterns: Option<Vec<String>>,
    pub media_want: Option<Option<serde_json::Value>>,
    pub match_mode: Option<String>,
    pub min_score: Option<f64>,
    pub enabled: Option<bool>,
}

/// Update a watch pattern by ID with only provided fields.
/// Uses dynamic SQL — left as a runtime query.
pub async fn update_pattern_partial<'e, E>(
    exec: E,
    id: Uuid,
    input: &WatchPatternPartialUpdate,
) -> Result<bool>
where
    E: Executor<'e, Database = Postgres>,
{
    // Build dynamic update query
    let mut updates = Vec::new();
    let mut bind_idx = 2; // $1 is reserved for id

    if input.patterns.is_some() {
        updates.push(format!("patterns = ${bind_idx}"));
        bind_idx += 1;
    }
    if input.media_want.is_some() {
        updates.push(format!("media_want = ${bind_idx}"));
        bind_idx += 1;
    }
    if input.match_mode.is_some() {
        updates.push(format!("match_mode = ${bind_idx}"));
        bind_idx += 1;
    }
    if input.min_score.is_some() {
        updates.push(format!("min_score = ${bind_idx}"));
        bind_idx += 1;
    }
    if input.enabled.is_some() {
        updates.push(format!("enabled = ${bind_idx}"));
    }

    if updates.is_empty() {
        return Ok(false);
    }

    updates.push("updated_at = NOW()".to_string());

    let query_str = format!(
        "UPDATE watch_patterns SET {} WHERE id = $1",
        updates.join(", ")
    );

    let mut query = sqlx::query(sqlx::AssertSqlSafe(query_str)).bind(id);

    if let Some(ref patterns) = input.patterns {
        query = query.bind(patterns);
    }
    if let Some(ref media_want) = input.media_want {
        query = query.bind(media_want);
    }
    if let Some(ref match_mode) = input.match_mode {
        query = query.bind(match_mode);
    }
    if let Some(min_score) = input.min_score {
        query = query.bind(min_score);
    }
    if let Some(enabled) = input.enabled {
        query = query.bind(enabled);
    }

    let result = query.execute(exec).await?;
    Ok(result.rows_affected() > 0)
}

/// Input for upserting a watch pattern from a NATS job.
#[derive(Debug)]
pub struct WatchPatternUpsertInput {
    pub source_system: String,
    pub source_id: String,
    pub callback_url: String,
    pub patterns: serde_json::Value,
    pub media_want: Option<serde_json::Value>,
    pub match_targets: serde_json::Value,
    pub match_mode: String,
    pub min_score: Option<f64>,
    pub media_type_id: Option<Uuid>,
    pub auto_download: bool,
    pub metadata: Option<serde_json::Value>,
    pub enabled: bool,
    pub upgrade_allowed: bool,
    pub search_not_before: Option<OffsetDateTime>,
}

/// Upsert a watch pattern from a NATS job (ON CONFLICT DO UPDATE).
/// Left as a runtime query because `patterns` is serde_json::Value bound to a text[] column.
pub async fn upsert_pattern_from_job<'e, E>(exec: E, input: WatchPatternUpsertInput) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query(
        r#"
        INSERT INTO watch_patterns (
            source_system, source_id, callback_url, patterns, media_want, match_targets, match_mode,
            min_score, media_type_id, auto_download, metadata, enabled, upgrade_allowed,
            search_not_before, created_at, updated_at
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, NOW(), NOW())
        ON CONFLICT (source_system, source_id) DO UPDATE SET
            callback_url = EXCLUDED.callback_url,
            patterns = EXCLUDED.patterns,
            media_want = EXCLUDED.media_want,
            match_targets = EXCLUDED.match_targets,
            match_mode = EXCLUDED.match_mode,
            min_score = EXCLUDED.min_score,
            media_type_id = EXCLUDED.media_type_id,
            auto_download = EXCLUDED.auto_download,
            metadata = EXCLUDED.metadata,
            enabled = EXCLUDED.enabled,
            upgrade_allowed = EXCLUDED.upgrade_allowed,
            search_not_before = EXCLUDED.search_not_before,
            upgrade_until = NULL,
            updated_at = NOW()
        "#,
    )
    .bind(&input.source_system)
    .bind(&input.source_id)
    .bind(&input.callback_url)
    .bind(&input.patterns)
    .bind(&input.media_want)
    .bind(&input.match_targets)
    .bind(&input.match_mode)
    .bind(input.min_score)
    .bind(input.media_type_id)
    .bind(input.auto_download)
    .bind(&input.metadata)
    .bind(input.enabled)
    .bind(input.upgrade_allowed)
    .bind(input.search_not_before)
    .execute(exec)
    .await?;
    Ok(())
}

/// Delete all patterns whose upgrade window has expired.
/// Returns the number of patterns deleted.
pub async fn delete_expired_upgrades<'e, E>(exec: E) -> Result<u64>
where
    E: Executor<'e, Database = Postgres>,
{
    let result = sqlx::query!(
        "DELETE FROM watch_patterns WHERE upgrade_until IS NOT NULL AND upgrade_until < NOW()"
    )
    .execute(exec)
    .await?;
    Ok(result.rows_affected())
}

/// Set `upgrade_until` — the post-coverage cleanup timer.
///
/// Note: this no longer gates upgrade eligibility (cutoff + margin do that,
/// Sonarr-style). The timestamp now exclusively drives `delete_expired_upgrades`
/// — patterns are removed entirely when this falls into the past, so kobbler
/// has to re-create them if it still wants the entity.
pub async fn set_upgrade_until<'e, E>(
    exec: E,
    pattern_id: Uuid,
    until: OffsetDateTime,
) -> Result<bool>
where
    E: Executor<'e, Database = Postgres>,
{
    let result = sqlx::query!(
        "UPDATE watch_patterns SET upgrade_until = $2 WHERE id = $1",
        pattern_id,
        until,
    )
    .execute(exec)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Clear `upgrade_until` on a pattern, returning it to "searching" status.
pub async fn clear_upgrade_until<'e, E>(exec: E, pattern_id: Uuid) -> Result<bool>
where
    E: Executor<'e, Database = Postgres>,
{
    let result = sqlx::query!(
        "UPDATE watch_patterns SET upgrade_until = NULL WHERE id = $1 AND upgrade_until IS NOT NULL",
        pattern_id,
    )
    .execute(exec)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Mark a pattern as eligible for immediate backlog re-pickup.
///
/// Writes `last_backlog_search_at = NOW() - (interval + 1 second)` instead of
/// NULL. This avoids re-opening the race window that NULL created: when a
/// pattern had `last_backlog_search_at = NULL`, the backlog scheduler treated
/// it as a never-searched fresh pattern and could fire concurrently with the
/// synchronous re-search that this same flow is about to run. By writing a
/// backdated timestamp the scheduler still picks it up on its next tick, but
/// the pattern is not mistaken for "brand new" (which now means "use
/// `created_at` as the baseline").
pub async fn reset_backlog_search<'e, E>(exec: E, pattern_id: Uuid) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query!(
        r#"
        UPDATE watch_patterns
        SET last_backlog_search_at = NOW()
            - (COALESCE(backlog_search_interval_hours, 24) * INTERVAL '1 hour')
            - INTERVAL '1 second'
        WHERE id = $1
        "#,
        pattern_id,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Lightweight row for orphan cleanup (avoids fetching all pattern columns).
#[derive(Debug, sqlx::FromRow)]
pub struct OrphanPatternRow {
    pub id: Uuid,
    pub source_system: String,
    pub source_id: String,
    pub upgrade_allowed: bool,
}

/// Find patterns that have at least one confirmed-imported download but were
/// never confirmed (no upgrade window set, not already in upgrade mode).
/// These are "orphaned" patterns whose confirm_want call was lost.
pub async fn find_orphaned_patterns<'e, E>(exec: E) -> Result<Vec<OrphanPatternRow>>
where
    E: Executor<'e, Database = Postgres>,
{
    let rows = sqlx::query_as!(
        OrphanPatternRow,
        r#"SELECT DISTINCT wp.id, wp.source_system, wp.source_id,
                  wp.upgrade_allowed as "upgrade_allowed!"
        FROM watch_patterns wp
        JOIN watch_matches wm ON wm.watch_pattern_id = wp.id
        WHERE wp.enabled = TRUE
          AND wp.upgrade_until IS NULL
          AND wm.rejected = FALSE
          AND wm.import_confirmed_at IS NOT NULL
          AND (wp.media_want IS NULL OR wp.media_want->>'kind' != 'tv_season')"#
    )
    .fetch_all(exec)
    .await?;
    Ok(rows)
}

/// Delete a single pattern by ID. Returns true if a row was deleted.
pub async fn delete_pattern_by_id<'e, E>(exec: E, pattern_id: Uuid) -> Result<bool>
where
    E: Executor<'e, Database = Postgres>,
{
    let result = sqlx::query!("DELETE FROM watch_patterns WHERE id = $1", pattern_id)
        .execute(exec)
        .await?;
    Ok(result.rows_affected() > 0)
}

/// Bulk delete watch patterns by IDs. Returns the number of rows deleted.
pub async fn bulk_delete_patterns<'e, E>(exec: E, ids: &[Uuid]) -> Result<u64>
where
    E: Executor<'e, Database = Postgres>,
{
    let result = sqlx::query!("DELETE FROM watch_patterns WHERE id = ANY($1)", ids)
        .execute(exec)
        .await?;
    Ok(result.rows_affected())
}

/// Append an entry to a pattern's `excluded_releases` JSON array.
/// Creates the array if it doesn't exist yet.
pub async fn append_excluded_release<'e, E>(
    exec: E,
    pattern_id: Uuid,
    entry: &serde_json::Value,
) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query!(
        r#"
        UPDATE watch_patterns
        SET excluded_releases = COALESCE(excluded_releases, '[]'::jsonb) || $2::jsonb,
            updated_at = NOW()
        WHERE id = $1
        "#,
        pattern_id,
        entry,
    )
    .execute(exec)
    .await?;
    Ok(())
}
