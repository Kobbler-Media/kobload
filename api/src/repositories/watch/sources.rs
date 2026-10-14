//! Indexer sources: CRUD operations.

use super::super::Result;
use sqlx::{Executor, Postgres};
use time::OffsetDateTime;
use uuid::Uuid;

/// Indexer source row from database.
#[derive(Debug, Clone)]
pub struct IndexerSourceRow {
    pub id: Uuid,
    pub name: String,
    pub handler_type: String,
    pub handler_config: serde_json::Value,
    pub category_ids: Option<Vec<i32>>,
    pub media_type_ids: Vec<Uuid>,
    pub enabled: bool,
    pub last_check_at: Option<OffsetDateTime>,
    pub last_error: Option<String>,
    pub state: Option<serde_json::Value>,
    pub created_at: OffsetDateTime,
    pub priority: i32,
    pub last_success_at: Option<OffsetDateTime>,
    pub last_failure_at: Option<OffsetDateTime>,
    pub consecutive_failures: i32,
    pub seed_ratio: Option<f64>,
    pub seed_time_minutes: Option<i32>,
    pub rate_limited_until: Option<OffsetDateTime>,
    pub auth_failed_at: Option<OffsetDateTime>,
    pub capabilities: Option<serde_json::Value>,
    pub caps_fetched_at: Option<OffsetDateTime>,
    pub api_limit_daily: Option<i32>,
    pub api_calls_today: i32,
    pub last_api_reset_at: OffsetDateTime,
    pub search_mode: String,
    pub auto_throttle: bool,
    pub debrid_mode: String,
}

/// Input for creating/updating an indexer source.
#[derive(Debug)]
pub struct IndexerSourceInput {
    pub name: String,
    pub handler_type: String,
    pub handler_config: serde_json::Value,
    pub category_ids: Option<Vec<i32>>,
    pub media_type_ids: Vec<Uuid>,
    pub enabled: bool,
    pub priority: i32,
    pub seed_ratio: Option<f64>,
    pub seed_time_minutes: Option<i32>,
    pub search_mode: Option<String>,
    pub auto_throttle: Option<bool>,
    pub api_limit_daily: Option<i32>,
    pub debrid_mode: Option<String>,
}

/// Input for updating an indexer source (all fields optional).
#[derive(Debug, Default)]
pub struct IndexerSourceUpdate {
    pub name: Option<String>,
    pub handler_type: Option<String>,
    pub handler_config: Option<serde_json::Value>,
    pub category_ids: Option<Option<Vec<i32>>>,
    pub media_type_ids: Option<Vec<Uuid>>,
    pub enabled: Option<bool>,
    pub priority: Option<i32>,
    pub seed_ratio: Option<Option<f64>>,
    pub seed_time_minutes: Option<Option<i32>>,
    pub search_mode: Option<String>,
    pub auto_throttle: Option<bool>,
    pub api_limit_daily: Option<Option<i32>>,
    pub debrid_mode: Option<String>,
}

/// List all indexer sources.
pub async fn list_indexer_sources<'e, E>(exec: E) -> Result<Vec<IndexerSourceRow>>
where
    E: Executor<'e, Database = Postgres>,
{
    let rows = sqlx::query_as!(
        IndexerSourceRow,
        r#"SELECT id, name, handler_type, handler_config, category_ids, media_type_ids,
               enabled as "enabled!", last_check_at, last_error, state,
               created_at as "created_at!", priority,
               last_success_at, last_failure_at, consecutive_failures,
               seed_ratio, seed_time_minutes, rate_limited_until, auth_failed_at,
               capabilities, caps_fetched_at, api_limit_daily,
               api_calls_today, last_api_reset_at as "last_api_reset_at!",
               search_mode as "search_mode!", auto_throttle as "auto_throttle!",
               debrid_mode as "debrid_mode!"
        FROM indexer_sources ORDER BY name"#
    )
    .fetch_all(exec)
    .await?;
    Ok(rows)
}

/// List enabled indexer sources.
pub async fn list_enabled_sources<'e, E>(exec: E) -> Result<Vec<IndexerSourceRow>>
where
    E: Executor<'e, Database = Postgres>,
{
    let rows = sqlx::query_as!(
        IndexerSourceRow,
        r#"SELECT id, name, handler_type, handler_config, category_ids, media_type_ids,
               enabled as "enabled!", last_check_at, last_error, state,
               created_at as "created_at!", priority,
               last_success_at, last_failure_at, consecutive_failures,
               seed_ratio, seed_time_minutes, rate_limited_until, auth_failed_at,
               capabilities, caps_fetched_at, api_limit_daily,
               api_calls_today, last_api_reset_at as "last_api_reset_at!",
               search_mode as "search_mode!", auto_throttle as "auto_throttle!",
               debrid_mode as "debrid_mode!"
        FROM indexer_sources WHERE enabled = true AND debrid_mode != 'always' ORDER BY priority DESC, name"#
    )
    .fetch_all(exec)
    .await?;
    Ok(rows)
}

/// List enabled indexer sources that support debrid streaming (debrid_mode 'always' or 'both').
pub async fn list_debrid_sources<'e, E>(exec: E) -> Result<Vec<IndexerSourceRow>>
where
    E: Executor<'e, Database = Postgres>,
{
    let rows = sqlx::query_as!(
        IndexerSourceRow,
        r#"SELECT id, name, handler_type, handler_config, category_ids, media_type_ids,
               enabled as "enabled!", last_check_at, last_error, state,
               created_at as "created_at!", priority,
               last_success_at, last_failure_at, consecutive_failures,
               seed_ratio, seed_time_minutes, rate_limited_until, auth_failed_at,
               capabilities, caps_fetched_at, api_limit_daily,
               api_calls_today, last_api_reset_at as "last_api_reset_at!",
               search_mode as "search_mode!", auto_throttle as "auto_throttle!",
               debrid_mode as "debrid_mode!"
        FROM indexer_sources WHERE enabled = true AND debrid_mode IN ('always', 'both') ORDER BY priority DESC, name"#
    )
    .fetch_all(exec)
    .await?;
    Ok(rows)
}

/// List indexer sources by handler type.
pub async fn list_indexer_sources_by_type<'e, E>(
    exec: E,
    handler_type: &str,
) -> Result<Vec<IndexerSourceRow>>
where
    E: Executor<'e, Database = Postgres>,
{
    let rows = sqlx::query_as!(
        IndexerSourceRow,
        r#"SELECT id, name, handler_type, handler_config, category_ids, media_type_ids,
               enabled as "enabled!", last_check_at, last_error, state,
               created_at as "created_at!", priority,
               last_success_at, last_failure_at, consecutive_failures,
               seed_ratio, seed_time_minutes, rate_limited_until, auth_failed_at,
               capabilities, caps_fetched_at, api_limit_daily,
               api_calls_today, last_api_reset_at as "last_api_reset_at!",
               search_mode as "search_mode!", auto_throttle as "auto_throttle!",
               debrid_mode as "debrid_mode!"
        FROM indexer_sources WHERE handler_type = $1"#,
        handler_type
    )
    .fetch_all(exec)
    .await?;
    Ok(rows)
}

/// Get indexer source by ID.
pub async fn get_indexer_source_by_id<'e, E>(exec: E, id: Uuid) -> Result<Option<IndexerSourceRow>>
where
    E: Executor<'e, Database = Postgres>,
{
    let row = sqlx::query_as!(
        IndexerSourceRow,
        r#"SELECT id, name, handler_type, handler_config, category_ids, media_type_ids,
               enabled as "enabled!", last_check_at, last_error, state,
               created_at as "created_at!", priority,
               last_success_at, last_failure_at, consecutive_failures,
               seed_ratio, seed_time_minutes, rate_limited_until, auth_failed_at,
               capabilities, caps_fetched_at, api_limit_daily,
               api_calls_today, last_api_reset_at as "last_api_reset_at!",
               search_mode as "search_mode!", auto_throttle as "auto_throttle!",
               debrid_mode as "debrid_mode!"
        FROM indexer_sources WHERE id = $1"#,
        id
    )
    .fetch_optional(exec)
    .await?;
    Ok(row)
}

/// Get enabled indexer sources due for check.
/// Excludes `prowlarr` parent sources (they sync to create `prowlarr_rss` children).
pub async fn get_sources_to_check<'e, E>(exec: E) -> Result<Vec<IndexerSourceRow>>
where
    E: Executor<'e, Database = Postgres>,
{
    let rows = sqlx::query_as!(
        IndexerSourceRow,
        r#"
        SELECT id, name, handler_type, handler_config, category_ids, media_type_ids,
               enabled as "enabled!", last_check_at, last_error, state,
               created_at as "created_at!", priority,
               last_success_at, last_failure_at, consecutive_failures,
               seed_ratio, seed_time_minutes, rate_limited_until, auth_failed_at,
               capabilities, caps_fetched_at, api_limit_daily,
               api_calls_today, last_api_reset_at as "last_api_reset_at!",
               search_mode as "search_mode!", auto_throttle as "auto_throttle!",
               debrid_mode as "debrid_mode!"
        FROM indexer_sources
        WHERE enabled = TRUE
          AND handler_type != 'prowlarr'
          AND debrid_mode != 'always'
          AND (
            last_check_at IS NULL
            OR last_check_at < NOW() - (
                COALESCE((handler_config->>'poll_interval_minutes')::int, 15) * INTERVAL '1 minute'
            )
          )
        "#
    )
    .fetch_all(exec)
    .await?;
    Ok(rows)
}

/// Create an indexer source.
pub async fn create_indexer_source<'e, E>(exec: E, input: IndexerSourceInput) -> Result<Uuid>
where
    E: Executor<'e, Database = Postgres>,
{
    let id = Uuid::now_v7();
    sqlx::query!(
        r#"
        INSERT INTO indexer_sources
            (id, name, handler_type, handler_config, category_ids, media_type_ids, enabled, priority,
             seed_ratio, seed_time_minutes, search_mode, auto_throttle, api_limit_daily, debrid_mode)
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10,
                COALESCE($11, 'full'), COALESCE($12, TRUE), $13, COALESCE($14, 'never'))
        "#,
        id,
        &input.name,
        &input.handler_type,
        &input.handler_config,
        input.category_ids.as_deref(),
        &input.media_type_ids,
        input.enabled,
        input.priority,
        input.seed_ratio as Option<f64>,
        input.seed_time_minutes as Option<i32>,
        input.search_mode.as_deref(),
        input.auto_throttle,
        input.api_limit_daily,
        input.debrid_mode.as_deref(),
    )
    .execute(exec)
    .await?;
    Ok(id)
}

/// Update an indexer source.
pub async fn update_indexer_source<'e, E>(
    exec: E,
    id: Uuid,
    input: IndexerSourceUpdate,
) -> Result<bool>
where
    E: Executor<'e, Database = Postgres>,
{
    let category_ids = input.category_ids.flatten();
    let seed_ratio = input.seed_ratio.flatten();
    let seed_time_minutes = input.seed_time_minutes.flatten();
    let api_limit_daily = input.api_limit_daily.flatten();
    let result = sqlx::query!(
        r#"
        UPDATE indexer_sources SET
            name = COALESCE($2, name),
            handler_type = COALESCE($3, handler_type),
            handler_config = COALESCE($4, handler_config),
            category_ids = COALESCE($5, category_ids),
            media_type_ids = COALESCE($6, media_type_ids),
            enabled = COALESCE($7, enabled),
            priority = COALESCE($8, priority),
            seed_ratio = COALESCE($9, seed_ratio),
            seed_time_minutes = COALESCE($10, seed_time_minutes),
            search_mode = COALESCE($11, search_mode),
            auto_throttle = COALESCE($12, auto_throttle),
            api_limit_daily = COALESCE($13, api_limit_daily),
            debrid_mode = COALESCE($14, debrid_mode)
        WHERE id = $1
        "#,
        id,
        input.name.as_deref(),
        input.handler_type.as_deref(),
        input.handler_config as Option<serde_json::Value>,
        category_ids.as_deref(),
        input.media_type_ids.as_deref(),
        input.enabled as Option<bool>,
        input.priority as Option<i32>,
        seed_ratio as Option<f64>,
        seed_time_minutes as Option<i32>,
        input.search_mode.as_deref(),
        input.auto_throttle,
        api_limit_daily,
        input.debrid_mode.as_deref(),
    )
    .execute(exec)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Update indexer source from Prowlarr sync.
pub async fn update_indexer_source_from_sync<'e, E>(
    exec: E,
    id: Uuid,
    name: &str,
    handler_config: &serde_json::Value,
    category_ids: Option<&Vec<i32>>,
) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query!(
        r#"
        UPDATE indexer_sources SET
            name = $2,
            handler_config = $3,
            category_ids = $4,
            enabled = TRUE
        WHERE id = $1
        "#,
        id,
        name,
        handler_config,
        category_ids.map(|v| v.as_slice()),
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Create indexer source from Prowlarr sync.
pub async fn create_indexer_source_from_sync<'e, E>(
    exec: E,
    name: &str,
    handler_config: &serde_json::Value,
    category_ids: Option<&Vec<i32>>,
) -> Result<Uuid>
where
    E: Executor<'e, Database = Postgres>,
{
    let id = Uuid::now_v7();
    sqlx::query!(
        r#"
        INSERT INTO indexer_sources
            (id, name, handler_type, handler_config, category_ids, enabled)
        VALUES ($1, $2, 'prowlarr_rss', $3, $4, TRUE)
        "#,
        id,
        name,
        handler_config,
        category_ids.map(|v| v.as_slice()),
    )
    .execute(exec)
    .await?;
    Ok(id)
}

/// Disable an indexer source.
pub async fn disable_indexer_source<'e, E>(exec: E, id: Uuid) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query!(
        "UPDATE indexer_sources SET enabled = FALSE WHERE id = $1",
        id
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Delete an indexer source.
pub async fn delete_indexer_source<'e, E>(exec: E, id: Uuid) -> Result<bool>
where
    E: Executor<'e, Database = Postgres>,
{
    let result = sqlx::query!("DELETE FROM indexer_sources WHERE id = $1", id)
        .execute(exec)
        .await?;
    Ok(result.rows_affected() > 0)
}

/// Reset last_check_at to trigger immediate check.
pub async fn trigger_indexer_check<'e, E>(exec: E, id: Uuid) -> Result<bool>
where
    E: Executor<'e, Database = Postgres>,
{
    let result = sqlx::query!(
        "UPDATE indexer_sources SET last_check_at = NULL WHERE id = $1",
        id
    )
    .execute(exec)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Reset health state and trigger immediate check.
/// Clears consecutive_failures, last_error, last_failure_at, last_check_at and
/// the auth-failure marker.
pub async fn reset_indexer_health<'e, E>(exec: E, id: Uuid) -> Result<bool>
where
    E: Executor<'e, Database = Postgres>,
{
    let result = sqlx::query!(
        r#"UPDATE indexer_sources
        SET consecutive_failures = 0,
            last_error = NULL,
            last_failure_at = NULL,
            last_check_at = NULL,
            rate_limited_until = NULL,
            auth_failed_at = NULL
        WHERE id = $1"#,
        id
    )
    .execute(exec)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// Update indexer source state after an RSS poll. This sets the RSS poll timer,
/// `last_check_at`.
pub async fn update_indexer_source_state<'e, E>(
    exec: E,
    id: Uuid,
    state: Option<&serde_json::Value>,
    error: Option<&str>,
) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    if let Some(err) = error {
        sqlx::query!(
            "UPDATE indexer_sources SET last_check_at = NOW(), last_error = $1 WHERE id = $2",
            err,
            id,
        )
        .execute(exec)
        .await?;
    } else {
        sqlx::query!(
            "UPDATE indexer_sources SET last_check_at = NOW(), last_error = NULL, state = COALESCE($1, state) WHERE id = $2",
            state as Option<&serde_json::Value>,
            id,
        )
        .execute(exec)
        .await?;
    }
    Ok(())
}

/// Check whether all enabled watch patterns were created after the most
/// recent RSS check. If so, RSS has never run with these patterns and
/// watermarks should be advanced to avoid a stale catch-up.
/// Returns false if there are no enabled patterns or no enabled sources.
pub async fn patterns_are_newer_than_last_rss(pool: &sqlx::PgPool) -> Result<bool> {
    let row = sqlx::query!(
        r#"
        SELECT
            (SELECT MIN(created_at) FROM watch_patterns WHERE enabled = TRUE) AS "earliest_pattern?",
            (SELECT MAX(last_check_at) FROM indexer_sources WHERE enabled = TRUE) AS "latest_check?"
        "#,
    )
    .fetch_one(pool)
    .await?;

    match (row.earliest_pattern, row.latest_check) {
        // Patterns exist but sources have never been checked → patterns are newer
        (Some(_), None) => Ok(true),
        // Patterns all created after the last RSS check → patterns are newer
        (Some(pattern), Some(check)) => Ok(pattern > check),
        // No patterns → nothing to do
        _ => Ok(false),
    }
}

/// Advance all source watermarks to now, so RSS starts fresh rather than
/// trying to catch up from a stale position. Called when watch patterns
/// are all newer than the last RSS check, since the initial backlog search
/// covers any releases that would have been caught by RSS.
pub async fn advance_all_watermarks_to_now(pool: &sqlx::PgPool) -> Result<()> {
    let now = time::OffsetDateTime::now_utc()
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default();
    let watermark = serde_json::json!({
        "last_seen_guid": null,
        "last_seen_publish_date": now,
    });
    sqlx::query!(
        "UPDATE indexer_sources SET state = $1 WHERE enabled = TRUE",
        &watermark,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Update last_check_at only (for successful check without state change).
pub async fn touch_indexer_source<'e, E>(exec: E, id: Uuid) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query!(
        "UPDATE indexer_sources SET last_check_at = NOW(), last_error = NULL WHERE id = $1",
        id,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Record a successful request to an indexer (resets failure tracking).
///
/// The `record_*` health helpers run for searches as well as RSS polls, so they
/// leave `last_check_at` alone: it is the RSS poll timer, set by
/// `update_indexer_source_state`. Bumping it on every search kept a heavily
/// searched indexer from ever looking due for RSS.
pub async fn record_success<'e, E>(exec: E, id: Uuid) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query!(
        r#"
        UPDATE indexer_sources SET
            last_success_at = NOW(),
            consecutive_failures = 0,
            last_error = NULL,
            rate_limited_until = NULL,
            auth_failed_at = NULL
        WHERE id = $1
        "#,
        id,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Record a failed request to an indexer (increments failure count).
pub async fn record_failure<'e, E>(exec: E, id: Uuid, error: &str) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query!(
        r#"
        UPDATE indexer_sources SET
            last_failure_at = NOW(),
            consecutive_failures = consecutive_failures + 1,
            last_error = $2
        WHERE id = $1
        "#,
        id,
        error,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Record a credential rejection (expired cookie/session, revoked API key).
///
/// Unlike `record_failure` this is not transient: retrying cannot fix it, so the
/// failure count is pinned at 1 to keep backoff short and cheap while
/// `auth_failed_at` carries the "needs a human" signal to the UI.
pub async fn record_auth_failure<'e, E>(exec: E, id: Uuid, error: &str) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query!(
        r#"
        UPDATE indexer_sources SET
            last_failure_at = NOW(),
            consecutive_failures = 1,
            last_error = $2,
            auth_failed_at = COALESCE(auth_failed_at, NOW())
        WHERE id = $1
        "#,
        id,
        error,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Maximum Retry-After we'll honour (12 hours).
const MAX_RETRY_AFTER_SECONDS: u64 = 12 * 3600;

/// Default backoff when a 429 has no Retry-After header (1 hour).
const DEFAULT_RETRY_AFTER_SECONDS: u64 = 3600;

/// Record a rate-limited (429) response. Uses the Retry-After value if
/// provided (capped at 12 hours), otherwise defaults to 1 hour.
///
/// Sets `rate_limited_until` so all code paths (RSS, search, backlog) skip
/// this source until the deadline expires.
///
/// Unlike `record_failure`, this resets `consecutive_failures` to 1 instead
/// of incrementing — a 429 is throttling, not a real error, and shouldn't
/// escalate to hours-long backoff.
pub async fn record_rate_limited<'e, E>(
    exec: E,
    id: Uuid,
    retry_after_seconds: Option<u64>,
) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    let cooldown = retry_after_seconds
        .unwrap_or(DEFAULT_RETRY_AFTER_SECONDS)
        .min(MAX_RETRY_AFTER_SECONDS) as i64;
    let msg = format!("rate limited, retry after {cooldown}s");
    sqlx::query!(
        r#"
        UPDATE indexer_sources SET
            last_failure_at = NOW(),
            consecutive_failures = 1,
            last_error = $2,
            rate_limited_until = NOW() + ($3 || ' seconds')::interval
        WHERE id = $1
        "#,
        id,
        msg,
        cooldown.to_string(),
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Check if an indexer should be skipped due to exponential backoff.
/// Backoff: 5min * 2^min(failures, 6) — max ~6 hours.
pub fn should_skip_backoff(source: &IndexerSourceRow) -> bool {
    if source.consecutive_failures == 0 {
        return false;
    }

    let Some(last_failure) = source.last_failure_at else {
        return false;
    };

    let exponent = source.consecutive_failures.min(6) as u32;
    let backoff_minutes = 5 * 2_i64.pow(exponent);
    let backoff = time::Duration::minutes(backoff_minutes);

    let now = OffsetDateTime::now_utc();
    now - last_failure < backoff
}

/// Check if an indexer is currently rate-limited (429 with known deadline).
pub fn is_rate_limited(source: &IndexerSourceRow) -> bool {
    source
        .rate_limited_until
        .is_some_and(|until| until > OffsetDateTime::now_utc())
}

/// Check if an indexer should be skipped (rate-limited OR exponential backoff).
pub fn should_skip_source(source: &IndexerSourceRow) -> bool {
    is_rate_limited(source) || should_skip_backoff(source)
}

/// Search context for throttling decisions.
#[derive(Debug, Clone, Copy)]
pub enum SearchContext {
    /// Interactive/manual search — never throttled by budget.
    Interactive,
    /// Backlog search — throttled when approaching budget limit.
    Backlog,
    /// RSS polling.
    Rss,
}

/// Check if a source should be skipped for a given search context.
/// Considers rate limiting, backoff, search_mode, API budget, and debrid mode.
pub fn should_skip_for_search(source: &IndexerSourceRow, context: &SearchContext) -> bool {
    // Debrid-only indexers are exclusively for stream/debrid searches.
    // They must not appear in RSS polling, backlog searches, or interactive searches.
    if source.debrid_mode == "always" {
        return true;
    }

    // Always check reactive rate limit and backoff
    if should_skip_source(source) {
        return true;
    }

    // Check search_mode
    match source.search_mode.as_str() {
        "rss_only" if !matches!(context, SearchContext::Rss) => {
            return true;
        }
        "backlog_only" if matches!(context, SearchContext::Rss) => {
            return true;
        }
        _ => {} // "full" — no restrictions, or mode matched the context
    }

    // Check API budget throttling (only for backlog searches when auto_throttle is on)
    if source.auto_throttle
        && matches!(context, SearchContext::Backlog)
        && let Some(limit) = source.api_limit_daily
        && limit > 0
    {
        let usage_pct = (source.api_calls_today as f64 / limit as f64) * 100.0;
        if usage_pct >= 90.0 {
            return true;
        }
    }

    false
}

/// Update priority for an indexer source.
pub async fn update_priority<'e, E>(exec: E, id: Uuid, priority: i32) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query!(
        "UPDATE indexer_sources SET priority = $2 WHERE id = $1",
        id,
        priority,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Count Newznab sources with stale or missing capabilities.
pub async fn count_stale_caps<'e, E>(exec: E) -> Result<i64>
where
    E: Executor<'e, Database = Postgres>,
{
    let (count,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM indexer_sources
         WHERE handler_type IN ('newznab', 'torznab') AND enabled = true
           AND (caps_fetched_at IS NULL OR caps_fetched_at < NOW() - INTERVAL '7 days')",
    )
    .fetch_one(exec)
    .await?;
    Ok(count)
}

/// Store fetched capabilities for an indexer source.
pub async fn update_indexer_caps<'e, E>(
    exec: E,
    id: Uuid,
    capabilities: &serde_json::Value,
) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query!(
        "UPDATE indexer_sources SET capabilities = $2, caps_fetched_at = NOW() WHERE id = $1",
        id,
        capabilities,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Increment API call counter, resetting if the 24-hour window has elapsed.
pub async fn increment_api_calls<'e, E>(exec: E, id: Uuid) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query!(
        r#"UPDATE indexer_sources SET
            api_calls_today = CASE
                WHEN last_api_reset_at < NOW() - INTERVAL '24 hours'
                THEN 1
                ELSE api_calls_today + 1
            END,
            last_api_reset_at = CASE
                WHEN last_api_reset_at < NOW() - INTERVAL '24 hours'
                THEN NOW()
                ELSE last_api_reset_at
            END
        WHERE id = $1"#,
        id,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Update API usage counters from a newznab response's apilimits element.
pub async fn update_api_limits_from_response<'e, E>(
    exec: E,
    id: Uuid,
    api_current: i32,
    api_max: i32,
) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query!(
        r#"UPDATE indexer_sources SET
            api_calls_today = $2,
            api_limit_daily = COALESCE(api_limit_daily, $3)
        WHERE id = $1"#,
        id,
        api_current,
        api_max,
    )
    .execute(exec)
    .await?;
    Ok(())
}
