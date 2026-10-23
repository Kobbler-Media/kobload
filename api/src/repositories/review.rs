use std::collections::HashSet;

use sqlx::{Executor, Postgres};
use time::OffsetDateTime;
use uuid::Uuid;

use super::Result;

/// `matched_pattern` of a grab made from the review queue, so the doubtful
/// check leaves the user's own choice alone.
pub const REVIEW_GRAB: &str = "review";

#[derive(Debug, Clone)]
pub struct ReviewItemRow {
    pub id: Uuid,
    pub watch_pattern_id: Uuid,
    pub kind: String,
    pub status: String,
    pub detail: serde_json::Value,
    pub created_at: OffsetDateTime,
}

#[derive(Debug, Clone)]
pub struct GrabRow {
    pub id: Uuid,
    pub watch_pattern_id: Uuid,
    pub release_guid: String,
    pub release_title: String,
    pub match_score: f64,
    pub matched_pattern: Option<String>,
}

pub async fn insert_item<'e, E>(
    exec: E,
    watch_pattern_id: Uuid,
    kind: &str,
    detail: &serde_json::Value,
) -> Result<Option<Uuid>>
where
    E: Executor<'e, Database = Postgres>,
{
    let id = sqlx::query_scalar!(
        r#"
        INSERT INTO review_items (watch_pattern_id, kind, detail)
        VALUES ($1, $2, $3)
        ON CONFLICT (watch_pattern_id, kind) DO NOTHING
        RETURNING id
        "#,
        watch_pattern_id,
        kind,
        detail,
    )
    .fetch_optional(exec)
    .await?;
    Ok(id)
}

pub async fn items_by_pattern<'e, E>(exec: E) -> Result<HashSet<(Uuid, String)>>
where
    E: Executor<'e, Database = Postgres>,
{
    let rows = sqlx::query!("SELECT watch_pattern_id, kind FROM review_items")
        .fetch_all(exec)
        .await?;
    Ok(rows
        .into_iter()
        .map(|r| (r.watch_pattern_id, r.kind))
        .collect())
}

pub async fn list_open<'e, E>(exec: E) -> Result<Vec<ReviewItemRow>>
where
    E: Executor<'e, Database = Postgres>,
{
    let rows = sqlx::query_as!(
        ReviewItemRow,
        r#"
        SELECT id, watch_pattern_id, kind, status, detail, created_at
        FROM review_items
        WHERE status = 'open'
        ORDER BY created_at DESC
        "#
    )
    .fetch_all(exec)
    .await?;
    Ok(rows)
}

pub async fn open_count<'e, E>(exec: E) -> Result<i64>
where
    E: Executor<'e, Database = Postgres>,
{
    let count = sqlx::query_scalar!(
        r#"SELECT COUNT(*) AS "count!" FROM review_items WHERE status = 'open'"#
    )
    .fetch_one(exec)
    .await?;
    Ok(count)
}

pub async fn get_item<'e, E>(exec: E, id: Uuid) -> Result<Option<ReviewItemRow>>
where
    E: Executor<'e, Database = Postgres>,
{
    let row = sqlx::query_as!(
        ReviewItemRow,
        r#"
        SELECT id, watch_pattern_id, kind, status, detail, created_at
        FROM review_items
        WHERE id = $1
        "#,
        id,
    )
    .fetch_optional(exec)
    .await?;
    Ok(row)
}

pub async fn update_detail<'e, E>(exec: E, id: Uuid, detail: &serde_json::Value) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query!(
        "UPDATE review_items SET detail = $2 WHERE id = $1",
        id,
        detail
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// `status` is `resolved` or `dismissed`.
pub async fn close<'e, E>(exec: E, id: Uuid, status: &str) -> Result<bool>
where
    E: Executor<'e, Database = Postgres>,
{
    let result = sqlx::query!(
        r#"
        UPDATE review_items SET status = $2, resolved_at = NOW()
        WHERE id = $1 AND status = 'open'
        "#,
        id,
        status,
    )
    .execute(exec)
    .await?;
    Ok(result.rows_affected() > 0)
}

/// A want that got something after all no longer needs its missed item.
pub async fn resolve_missed_with_grabs<'e, E>(exec: E) -> Result<u64>
where
    E: Executor<'e, Database = Postgres>,
{
    let result = sqlx::query!(
        r#"
        UPDATE review_items r SET status = 'resolved', resolved_at = NOW()
        WHERE r.status = 'open' AND r.kind = 'missed'
          AND EXISTS (
              SELECT 1 FROM watch_matches m
              WHERE m.watch_pattern_id = r.watch_pattern_id
                AND m.action IN ('downloaded', 'upgrade', 'held')
                AND NOT m.rejected
          )
        "#
    )
    .execute(exec)
    .await?;
    Ok(result.rows_affected())
}

pub async fn patterns_with_grab<'e, E>(exec: E) -> Result<HashSet<Uuid>>
where
    E: Executor<'e, Database = Postgres>,
{
    let rows = sqlx::query_scalar!(
        r#"
        SELECT DISTINCT watch_pattern_id AS "id!" FROM watch_matches
        WHERE action IN ('downloaded', 'upgrade', 'held') AND NOT rejected
          AND watch_pattern_id IS NOT NULL
        "#
    )
    .fetch_all(exec)
    .await?;
    Ok(rows.into_iter().collect())
}

pub async fn grabs_since<'e, E>(exec: E, since: OffsetDateTime) -> Result<Vec<GrabRow>>
where
    E: Executor<'e, Database = Postgres>,
{
    let rows = sqlx::query_as!(
        GrabRow,
        r#"
        SELECT id, watch_pattern_id AS "watch_pattern_id!", release_guid, release_title,
               match_score, matched_pattern
        FROM watch_matches
        WHERE action IN ('downloaded', 'upgrade') AND NOT rejected
          AND watch_pattern_id IS NOT NULL
          AND created_at > $1
        ORDER BY created_at
        "#,
        since,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows)
}

pub async fn reject<'e, E>(exec: E, watch_pattern_id: Uuid, release_key: &str) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query!(
        r#"
        INSERT INTO review_rejections (watch_pattern_id, release_key)
        VALUES ($1, $2)
        ON CONFLICT DO NOTHING
        "#,
        watch_pattern_id,
        release_key,
    )
    .execute(exec)
    .await?;
    Ok(())
}

pub async fn rejected_keys<'e, E>(exec: E, watch_pattern_id: Uuid) -> Result<HashSet<String>>
where
    E: Executor<'e, Database = Postgres>,
{
    let rows = sqlx::query_scalar!(
        "SELECT release_key FROM review_rejections WHERE watch_pattern_id = $1",
        watch_pattern_id,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows.into_iter().collect())
}
