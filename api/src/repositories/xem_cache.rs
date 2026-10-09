use sqlx::{Executor, Postgres};
use time::OffsetDateTime;

use super::Result;

pub struct XemCacheRow {
    pub scene_mappings: serde_json::Value,
    pub fetched_at: OffsetDateTime,
}

/// Look up cached XEM show mappings by TVDB ID.
pub async fn get<'e, E>(exec: E, tvdb_id: i64) -> Result<Option<XemCacheRow>>
where
    E: Executor<'e, Database = Postgres>,
{
    let row = sqlx::query_as!(
        XemCacheRow,
        "SELECT scene_mappings, fetched_at FROM xem_cache WHERE tvdb_id = $1",
        tvdb_id,
    )
    .fetch_optional(exec)
    .await?;

    Ok(row)
}

/// Store XEM show mappings in the cache (upsert).
pub async fn put<'e, E>(exec: E, tvdb_id: i64, scene_mappings: &serde_json::Value) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query!(
        r#"
        INSERT INTO xem_cache (tvdb_id, scene_mappings, fetched_at)
        VALUES ($1, $2, NOW())
        ON CONFLICT (tvdb_id) DO UPDATE SET
            scene_mappings = EXCLUDED.scene_mappings,
            fetched_at = EXCLUDED.fetched_at
        "#,
        tvdb_id,
        scene_mappings,
    )
    .execute(exec)
    .await?;

    Ok(())
}

/// Bump fetched_at to now without changing the cached data (used on refresh failure).
pub async fn touch<'e, E>(exec: E, tvdb_id: i64) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query!(
        "UPDATE xem_cache SET fetched_at = NOW() WHERE tvdb_id = $1",
        tvdb_id,
    )
    .execute(exec)
    .await?;

    Ok(())
}

/// Get all cached TVDB IDs (for warm-starting the havemap).
pub async fn all_tvdb_ids<'e, E>(exec: E) -> Result<Vec<i64>>
where
    E: Executor<'e, Database = Postgres>,
{
    let rows = sqlx::query_scalar!("SELECT tvdb_id FROM xem_cache ORDER BY tvdb_id")
        .fetch_all(exec)
        .await?;

    Ok(rows)
}

/// Delete cache entries older than `max_age_hours`.
pub async fn prune_stale<'e, E>(exec: E, max_age_hours: i64) -> Result<u64>
where
    E: Executor<'e, Database = Postgres>,
{
    let result = sqlx::query!(
        "DELETE FROM xem_cache WHERE fetched_at < NOW() - make_interval(hours => $1)",
        max_age_hours as i32,
    )
    .execute(exec)
    .await?;

    Ok(result.rows_affected())
}
