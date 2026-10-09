use sqlx::{PgPool, Row};
use time::OffsetDateTime;

use super::Result;

pub struct TrashFilterRow {
    pub category: String,
    pub spec_name: String,
    pub filter_type: String,
    pub pattern: String,
    pub negate: bool,
    pub required: bool,
    pub tier_bonus: Option<f64>,
}

/// Load all trash filter rows from the cache.
pub async fn get_all(pool: &PgPool) -> Result<Vec<TrashFilterRow>> {
    let rows = sqlx::query(
        "SELECT category, spec_name, filter_type, pattern, negate, required, tier_bonus FROM trash_filter_cache",
    )
    .fetch_all(pool)
    .await?;

    Ok(rows
        .into_iter()
        .map(|r| TrashFilterRow {
            category: r.get("category"),
            spec_name: r.get("spec_name"),
            filter_type: r.get("filter_type"),
            pattern: r.get("pattern"),
            negate: r.get("negate"),
            required: r.get("required"),
            tier_bonus: r.get("tier_bonus"),
        })
        .collect())
}

/// Replace all trash filter rows in a single transaction.
pub async fn replace_all(pool: &PgPool, rows: &[TrashFilterRow]) -> Result<()> {
    let mut tx = pool.begin().await?;

    sqlx::query("DELETE FROM trash_filter_cache")
        .execute(&mut *tx)
        .await?;

    for row in rows {
        sqlx::query(
            "INSERT INTO trash_filter_cache (category, spec_name, filter_type, pattern, negate, required, tier_bonus) VALUES ($1, $2, $3, $4, $5, $6, $7)",
        )
        .bind(&row.category)
        .bind(&row.spec_name)
        .bind(&row.filter_type)
        .bind(&row.pattern)
        .bind(row.negate)
        .bind(row.required)
        .bind(row.tier_bonus)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;
    Ok(())
}

/// Get the last-fetched timestamp from trash_filter_meta.
pub async fn get_last_fetched(pool: &PgPool) -> Result<Option<OffsetDateTime>> {
    let row: Option<(OffsetDateTime,)> =
        sqlx::query_as("SELECT value_at FROM trash_filter_meta WHERE key = 'last_fetched'")
            .fetch_optional(pool)
            .await?;

    Ok(row.map(|r| r.0))
}

/// Upsert the last-fetched timestamp in trash_filter_meta.
pub async fn set_last_fetched(pool: &PgPool) -> Result<()> {
    sqlx::query(
        r#"
        INSERT INTO trash_filter_meta (key, value_at)
        VALUES ('last_fetched', NOW())
        ON CONFLICT (key) DO UPDATE SET value_at = EXCLUDED.value_at
        "#,
    )
    .execute(pool)
    .await?;

    Ok(())
}
