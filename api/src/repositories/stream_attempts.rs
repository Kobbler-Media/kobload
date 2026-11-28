use serde::Serialize;
use sqlx::PgPool;
use time::OffsetDateTime;
use uuid::Uuid;

use super::Result;

pub const RETENTION_HOURS: i32 = 24;
pub const STALE_MINUTES: i32 = 10;

#[derive(Debug, Clone, Serialize)]
pub struct StreamAttempt {
    pub id: Uuid,
    pub provider_id: Uuid,
    pub provider_name: String,
    pub search_id: Option<Uuid>,
    pub origin: String,
    pub release_key: String,
    pub file_id: Option<i32>,
    pub release_title: Option<String>,
    pub file_name: Option<String>,
    pub status: String,
    pub cached: Option<bool>,
    pub progress: Option<f32>,
    pub speed: Option<i64>,
    pub torrent_id: Option<String>,
    pub error: Option<String>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub updated_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339::option")]
    pub ready_at: Option<OffsetDateTime>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StreamAttemptEvent {
    pub id: Uuid,
    pub event_type: String,
    pub message: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

pub struct NewAttempt<'a> {
    pub provider_id: Uuid,
    pub search_id: Option<Uuid>,
    pub origin: &'a str,
    pub release_key: &'a str,
    pub file_id: Option<i32>,
    pub release_title: Option<&'a str>,
    pub file_name: Option<&'a str>,
}

#[derive(Debug, Default, Clone)]
pub struct Change {
    pub status: Option<&'static str>,
    pub cached: Option<bool>,
    pub progress: Option<f32>,
    pub speed: Option<i64>,
    pub torrent_id: Option<String>,
    pub error: Option<String>,
    pub release_key: Option<String>,
}

impl Change {
    pub fn failed(error: &str) -> Self {
        Self {
            status: Some("failed"),
            error: Some(error.to_string()),
            ..Default::default()
        }
    }
}

#[derive(Debug, Default)]
pub struct ListFilter<'a> {
    /// Attempts with these ids, or on these searches.
    pub members: Option<(&'a [Uuid], &'a [Uuid])>,
    pub status: Option<&'a str>,
    pub search: Option<&'a str>,
    pub before: Option<OffsetDateTime>,
    pub limit: i64,
}

pub async fn insert(pool: &PgPool, new: &NewAttempt<'_>) -> Result<Uuid> {
    let id = sqlx::query_scalar!(
        r#"INSERT INTO stream_attempts (provider_id, origin, release_key, file_id, release_title, file_name, search_id)
           VALUES ($1, $2, $3, $4, $5, $6, $7)
           RETURNING id"#,
        new.provider_id,
        new.origin,
        new.release_key,
        new.file_id,
        new.release_title,
        new.file_name,
        new.search_id,
    )
    .fetch_one(pool)
    .await?;
    Ok(id)
}

pub async fn update(pool: &PgPool, id: Uuid, change: &Change) -> Result<()> {
    sqlx::query!(
        r#"UPDATE stream_attempts SET
               status = COALESCE($2, status),
               cached = COALESCE($3, cached),
               progress = COALESCE($4, progress),
               torrent_id = COALESCE($5, torrent_id),
               error = COALESCE($6, error),
               release_key = COALESCE($7, release_key),
               speed = CASE WHEN $2 IS NOT NULL AND $2 <> 'caching' THEN NULL ELSE COALESCE($8, speed) END,
               ready_at = CASE WHEN $2 = 'ready' THEN COALESCE(ready_at, NOW()) ELSE ready_at END,
               updated_at = NOW()
           WHERE id = $1"#,
        id,
        change.status,
        change.cached,
        change.progress,
        change.torrent_id,
        change.error,
        change.release_key,
        change.speed,
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn add_event(pool: &PgPool, id: Uuid, event_type: &str, message: &str) -> Result<()> {
    sqlx::query!(
        "INSERT INTO stream_attempt_events (attempt_id, event_type, message) VALUES ($1, $2, $3)",
        id,
        event_type,
        message,
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn list(pool: &PgPool, filter: &ListFilter<'_>) -> Result<Vec<StreamAttempt>> {
    let search = filter.search.map(|s| format!("%{s}%"));
    let rows = sqlx::query_as!(
        StreamAttempt,
        r#"SELECT a.id, a.provider_id, p.name AS provider_name, a.search_id, a.origin, a.release_key,
                  a.file_id, a.release_title, a.file_name, a.status, a.cached, a.progress, a.speed,
                  a.torrent_id, a.error, a.created_at, a.updated_at, a.ready_at
           FROM stream_attempts a
           JOIN debrid_providers p ON p.id = a.provider_id
           WHERE ($1::uuid[] IS NULL OR a.id = ANY($1) OR a.search_id = ANY($6))
             AND ($2::text IS NULL OR a.status = $2)
             AND ($3::text IS NULL OR a.release_title ILIKE $3 OR a.file_name ILIKE $3
                  OR a.release_key ILIKE $3)
             AND ($4::timestamptz IS NULL OR a.created_at < $4)
           ORDER BY a.created_at DESC
           LIMIT $5"#,
        filter.members.map(|(ids, _)| ids),
        filter.status,
        search,
        filter.before,
        filter.limit,
        filter.members.map_or(&[][..], |(_, searches)| searches),
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

pub struct Head {
    pub id: Uuid,
    pub is_search: bool,
}

/// The newest searches and the attempts no search listed, interleaved by time: one head per
/// row of the grouped debrid log.
pub async fn heads(
    pool: &PgPool,
    search: Option<&str>,
    before: Option<OffsetDateTime>,
    limit: i64,
) -> Result<Vec<Head>> {
    let search = search.map(|s| format!("%{s}%"));
    let rows = sqlx::query!(
        r#"SELECT id AS "id!", is_search AS "is_search!" FROM (
               SELECT s.id, s.created_at, true AS is_search
               FROM stream_searches s
               WHERE ($2::timestamptz IS NULL OR s.created_at < $2)
                 AND ($1::text IS NULL OR s.title ILIKE $1 OR EXISTS (
                     SELECT 1 FROM jsonb_array_elements(s.options) o
                     WHERE o->>'release_title' ILIKE $1 OR o->>'release_key' ILIKE $1))
               UNION ALL
               SELECT a.id, a.created_at, false
               FROM stream_attempts a
               WHERE a.search_id IS NULL
                 AND ($2::timestamptz IS NULL OR a.created_at < $2)
                 AND ($1::text IS NULL OR a.release_title ILIKE $1 OR a.file_name ILIKE $1
                      OR a.release_key ILIKE $1)
           ) heads
           ORDER BY created_at DESC
           LIMIT $3"#,
        search,
        before,
        limit,
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| Head {
            id: r.id,
            is_search: r.is_search,
        })
        .collect())
}

pub async fn get(pool: &PgPool, id: Uuid) -> Result<Option<StreamAttempt>> {
    let ids = [id];
    let filter = ListFilter {
        members: Some((&ids, &[])),
        limit: 1,
        ..Default::default()
    };
    Ok(list(pool, &filter).await?.pop())
}

pub async fn events(pool: &PgPool, id: Uuid) -> Result<Vec<StreamAttemptEvent>> {
    let rows = sqlx::query_as!(
        StreamAttemptEvent,
        r#"SELECT id, event_type, message, created_at
           FROM stream_attempt_events
           WHERE attempt_id = $1
           ORDER BY created_at, id"#,
        id,
    )
    .fetch_all(pool)
    .await?;
    Ok(rows)
}

pub async fn caching_for_torrent(
    pool: &PgPool,
    provider_id: Uuid,
    torrent_id: &str,
) -> Result<Vec<Uuid>> {
    let ids = sqlx::query_scalar!(
        r#"SELECT id FROM stream_attempts
           WHERE provider_id = $1 AND torrent_id = $2 AND status = 'caching'"#,
        provider_id,
        torrent_id,
    )
    .fetch_all(pool)
    .await?;
    Ok(ids)
}

pub async fn latest_for_release(
    pool: &PgPool,
    provider_id: Option<Uuid>,
    release_key: &str,
    file_id: Option<i32>,
) -> Result<Option<Uuid>> {
    let id = sqlx::query_scalar!(
        r#"SELECT id FROM stream_attempts
           WHERE release_key = $1
             AND ($2::uuid IS NULL OR provider_id = $2)
             AND status IN ('ready', 'caching', 'played')
             AND ($3::int IS NULL OR file_id IS NULL OR file_id = $3)
           ORDER BY (file_id IS NOT DISTINCT FROM $3) DESC, (origin = 'play') DESC, created_at DESC
           LIMIT 1"#,
        release_key,
        provider_id,
        file_id,
    )
    .fetch_optional(pool)
    .await?;
    Ok(id)
}

pub async fn abandon_stale(pool: &PgPool, minutes: i32, message: &str) -> Result<u64> {
    let result = sqlx::query!(
        r#"WITH stale AS (
               UPDATE stream_attempts
               SET status = 'abandoned', error = COALESCE(error, $2), updated_at = NOW()
               WHERE status IN ('resolving', 'caching')
                 AND updated_at < NOW() - make_interval(mins => $1)
               RETURNING id
           )
           INSERT INTO stream_attempt_events (attempt_id, event_type, message)
           SELECT id, 'abandoned', $2 FROM stale"#,
        minutes,
        message,
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

pub async fn prune(pool: &PgPool, hours: i32) -> Result<u64> {
    let result = sqlx::query!(
        "DELETE FROM stream_attempts WHERE created_at < NOW() - make_interval(hours => $1)",
        hours,
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

pub async fn clear(pool: &PgPool) -> Result<u64> {
    let result = sqlx::query!("DELETE FROM stream_attempts")
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}
