use sqlx::{PgPool, Row};

use super::Result;

pub struct XemTitleRow {
    pub tvdb_id: i64,
    pub title: String,
    pub season: i32,
}

/// Get all cached XEM titles (for warm-starting the in-memory cache).
pub async fn get_all(pool: &PgPool) -> Result<Vec<XemTitleRow>> {
    let rows = sqlx::query("SELECT tvdb_id, title, season FROM xem_title_cache")
        .fetch_all(pool)
        .await?;

    Ok(rows
        .into_iter()
        .map(|r| XemTitleRow {
            tvdb_id: r.get("tvdb_id"),
            title: r.get("title"),
            season: r.get("season"),
        })
        .collect())
}

/// Get cached XEM titles for a TVDB ID.
pub async fn get_titles(pool: &PgPool, tvdb_id: i64) -> Result<Vec<XemTitleRow>> {
    let rows = sqlx::query("SELECT tvdb_id, title, season FROM xem_title_cache WHERE tvdb_id = $1")
        .bind(tvdb_id)
        .fetch_all(pool)
        .await?;

    Ok(rows
        .into_iter()
        .map(|r| XemTitleRow {
            tvdb_id: r.get("tvdb_id"),
            title: r.get("title"),
            season: r.get("season"),
        })
        .collect())
}

/// Replace all XEM titles for a TVDB ID (delete + bulk insert).
pub async fn put_titles(pool: &PgPool, tvdb_id: i64, titles: &[(String, i32)]) -> Result<()> {
    let mut tx = pool.begin().await?;

    sqlx::query("DELETE FROM xem_title_cache WHERE tvdb_id = $1")
        .bind(tvdb_id)
        .execute(&mut *tx)
        .await?;

    for (title, season) in titles {
        sqlx::query(
            "INSERT INTO xem_title_cache (tvdb_id, title, season, fetched_at) VALUES ($1, $2, $3, NOW())",
        )
        .bind(tvdb_id)
        .bind(title)
        .bind(*season)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;
    Ok(())
}

/// Store all titles from a full dataset fetch.
pub async fn put_all(pool: &PgPool, all: &[(i64, String, i32)]) -> Result<()> {
    let mut tx = pool.begin().await?;

    sqlx::query("DELETE FROM xem_title_cache")
        .execute(&mut *tx)
        .await?;

    for (tvdb_id, title, season) in all {
        sqlx::query(
            "INSERT INTO xem_title_cache (tvdb_id, title, season, fetched_at) VALUES ($1, $2, $3, NOW())",
        )
        .bind(*tvdb_id)
        .bind(title)
        .bind(*season)
        .execute(&mut *tx)
        .await?;
    }

    tx.commit().await?;
    Ok(())
}
