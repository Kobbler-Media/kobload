use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use sqlx::types::Json;
use time::OffsetDateTime;
use uuid::Uuid;

use super::Result;

/// A play is linked to the newest search that listed its release within this window.
const LINK_HOURS: i32 = 6;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchOption {
    pub rank: usize,
    pub release_key: Option<String>,
    pub file_id: Option<i32>,
    pub release_title: String,
    pub size: i64,
    pub indexer: String,
    pub source_type: String,
    pub group: Option<String>,
    pub availability: Option<String>,
    pub total_score: f64,
    pub why: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StreamSearch {
    pub id: Uuid,
    pub title: String,
    pub options: Vec<SearchOption>,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

pub async fn insert(
    pool: &PgPool,
    id: Uuid,
    title: &str,
    options: &[SearchOption],
) -> Result<StreamSearch> {
    let row = sqlx::query!(
        r#"INSERT INTO stream_searches (id, title, options) VALUES ($1, $2, $3)
           RETURNING created_at"#,
        id,
        title,
        Json(options) as _,
    )
    .fetch_one(pool)
    .await?;
    Ok(StreamSearch {
        id,
        title: title.to_string(),
        options: options.to_vec(),
        created_at: row.created_at,
    })
}

/// The newest recent search that listed this release, by key or, for an NZB imported since
/// it was listed under its indexer's reference, by title.
pub async fn listing(
    pool: &PgPool,
    release_key: &str,
    release_title: Option<&str>,
) -> Result<Option<Uuid>> {
    let id = sqlx::query_scalar!(
        r#"SELECT s.id FROM stream_searches s
           WHERE s.created_at > NOW() - make_interval(hours => $3)
             AND EXISTS (
                 SELECT 1 FROM jsonb_array_elements(s.options) o
                 WHERE lower(o->>'release_key') = lower($1)
                    OR ($2::text IS NOT NULL AND o->>'release_title' = $2)
             )
           ORDER BY s.created_at DESC
           LIMIT 1"#,
        release_key,
        release_title,
        LINK_HOURS,
    )
    .fetch_optional(pool)
    .await?;
    Ok(id)
}

pub async fn get_many(pool: &PgPool, ids: &[Uuid]) -> Result<Vec<StreamSearch>> {
    let rows = sqlx::query!(
        r#"SELECT id, title, options AS "options: Json<Vec<SearchOption>>", created_at
           FROM stream_searches WHERE id = ANY($1)"#,
        ids,
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| StreamSearch {
            id: r.id,
            title: r.title,
            options: r.options.0,
            created_at: r.created_at,
        })
        .collect())
}

pub async fn prune(pool: &PgPool, hours: i32) -> Result<u64> {
    let result = sqlx::query!(
        "DELETE FROM stream_searches WHERE created_at < NOW() - make_interval(hours => $1)",
        hours,
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}

pub async fn clear(pool: &PgPool) -> Result<u64> {
    let result = sqlx::query!("DELETE FROM stream_searches")
        .execute(pool)
        .await?;
    Ok(result.rows_affected())
}
