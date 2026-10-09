//! Indexer RSS cache repository.

use super::Result;
use sqlx::{Executor, Postgres};
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct IndexerRssCacheRow {
    pub indexer_source_id: Uuid,
    pub fetched_at: OffsetDateTime,
    pub ttl_seconds: i32,
    pub response_xml: String,
}

pub async fn get_rss_cache<'e, E>(
    exec: E,
    indexer_source_id: Uuid,
) -> Result<Option<IndexerRssCacheRow>>
where
    E: Executor<'e, Database = Postgres>,
{
    let row = sqlx::query_as!(
        IndexerRssCacheRow,
        r#"
        SELECT indexer_source_id, fetched_at, ttl_seconds, response_xml
        FROM indexer_rss_cache
        WHERE indexer_source_id = $1
        "#,
        indexer_source_id,
    )
    .fetch_optional(exec)
    .await?;
    Ok(row)
}

pub async fn upsert_rss_cache<'e, E>(
    exec: E,
    indexer_source_id: Uuid,
    fetched_at: OffsetDateTime,
    ttl_seconds: i32,
    response_xml: &str,
) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query!(
        r#"
        INSERT INTO indexer_rss_cache (indexer_source_id, fetched_at, ttl_seconds, response_xml)
        VALUES ($1, $2, $3, $4)
        ON CONFLICT (indexer_source_id) DO UPDATE SET
            fetched_at = EXCLUDED.fetched_at,
            ttl_seconds = EXCLUDED.ttl_seconds,
            response_xml = EXCLUDED.response_xml
        "#,
        indexer_source_id,
        fetched_at,
        ttl_seconds,
        response_xml,
    )
    .execute(exec)
    .await?;
    Ok(())
}

pub async fn delete_rss_cache<'e, E>(exec: E, indexer_source_id: Uuid) -> Result<bool>
where
    E: Executor<'e, Database = Postgres>,
{
    let result = sqlx::query!(
        "DELETE FROM indexer_rss_cache WHERE indexer_source_id = $1",
        indexer_source_id
    )
    .execute(exec)
    .await?;
    Ok(result.rows_affected() > 0)
}
