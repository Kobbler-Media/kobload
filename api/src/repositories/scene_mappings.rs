use sqlx::{PgPool, Row};
use time::OffsetDateTime;

use super::Result;

pub struct SceneMappingRow {
    pub tvdb_id: i32,
    pub scene_title: String,
    pub season: i32,
    pub filter_regex: Option<String>,
}

/// Load all scene mapping rows from the cache.
pub async fn get_all(pool: &PgPool) -> Result<Vec<SceneMappingRow>> {
    let rows =
        sqlx::query("SELECT tvdb_id, scene_title, season, filter_regex FROM scene_mapping_cache")
            .fetch_all(pool)
            .await?;

    Ok(rows
        .into_iter()
        .map(|r| SceneMappingRow {
            tvdb_id: r.get("tvdb_id"),
            scene_title: r.get("scene_title"),
            season: r.get("season"),
            filter_regex: r.get("filter_regex"),
        })
        .collect())
}

/// Replace all scene mapping rows in a single transaction.
pub async fn replace_all(pool: &PgPool, rows: &[(i64, &str, i32, Option<&str>)]) -> Result<()> {
    let mut tx = pool.begin().await?;

    sqlx::query("DELETE FROM scene_mapping_cache")
        .execute(&mut *tx)
        .await?;

    for &(tvdb_id, title, season, filter_regex) in rows {
        sqlx::query(
            "INSERT INTO scene_mapping_cache (tvdb_id, scene_title, season, filter_regex) VALUES ($1, $2, $3, $4) ON CONFLICT (tvdb_id, scene_title, season) DO NOTHING",
        )
        .bind(tvdb_id as i32)
        .bind(title)
        .bind(season)
        .bind(filter_regex)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;
    Ok(())
}

/// Get the last-fetched timestamp from scene_mapping_meta.
pub async fn get_last_fetched(pool: &PgPool) -> Result<Option<OffsetDateTime>> {
    let row: Option<(OffsetDateTime,)> =
        sqlx::query_as("SELECT value_at FROM scene_mapping_meta WHERE key = 'last_fetched'")
            .fetch_optional(pool)
            .await?;

    Ok(row.map(|r| r.0))
}

/// Upsert the last-fetched timestamp in scene_mapping_meta.
pub async fn set_last_fetched(pool: &PgPool) -> Result<()> {
    sqlx::query(
        r#"
        INSERT INTO scene_mapping_meta (key, value_at)
        VALUES ('last_fetched', NOW())
        ON CONFLICT (key) DO UPDATE SET value_at = EXCLUDED.value_at
        "#,
    )
    .execute(pool)
    .await?;

    Ok(())
}
