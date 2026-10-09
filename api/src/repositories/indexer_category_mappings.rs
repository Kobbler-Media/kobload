//! Indexer category mappings repository.

use super::Result;
use serde::Serialize;
use sqlx::{Executor, Postgres};
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize)]
pub struct IndexerCategoryMappingRow {
    pub id: Uuid,
    pub indexer_source_id: Uuid,
    pub media_type_id: Uuid,
    pub indexer_category_id: i32,
    pub created_at: OffsetDateTime,
}

#[derive(Debug)]
pub struct IndexerCategoryMappingInput {
    pub indexer_source_id: Uuid,
    pub media_type_id: Uuid,
    pub indexer_category_id: i32,
}

pub async fn list_category_mappings_by_source<'e, E>(
    exec: E,
    indexer_source_id: Uuid,
) -> Result<Vec<IndexerCategoryMappingRow>>
where
    E: Executor<'e, Database = Postgres>,
{
    let rows = sqlx::query_as!(
        IndexerCategoryMappingRow,
        r#"
        SELECT id, indexer_source_id, media_type_id as "media_type_id!", indexer_category_id, created_at
        FROM indexer_category_mappings
        WHERE indexer_source_id = $1
        ORDER BY media_type_id, indexer_category_id
        "#,
        indexer_source_id,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows)
}

pub async fn list_category_mappings_by_media_type<'e, E>(
    exec: E,
    indexer_source_id: Uuid,
    media_type_id: Uuid,
) -> Result<Vec<IndexerCategoryMappingRow>>
where
    E: Executor<'e, Database = Postgres>,
{
    let rows = sqlx::query_as!(
        IndexerCategoryMappingRow,
        r#"
        SELECT id, indexer_source_id, media_type_id as "media_type_id!", indexer_category_id, created_at
        FROM indexer_category_mappings
        WHERE indexer_source_id = $1 AND media_type_id = $2
        ORDER BY indexer_category_id
        "#,
        indexer_source_id,
        media_type_id,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows)
}

pub async fn create_category_mapping<'e, E>(
    exec: E,
    input: IndexerCategoryMappingInput,
) -> Result<Uuid>
where
    E: Executor<'e, Database = Postgres>,
{
    let id = Uuid::now_v7();
    sqlx::query!(
        r#"
        INSERT INTO indexer_category_mappings (id, indexer_source_id, media_type_id, indexer_category_id)
        VALUES ($1, $2, $3, $4)
        ON CONFLICT (indexer_source_id, media_type_id, indexer_category_id) DO NOTHING
        "#,
        id,
        input.indexer_source_id,
        input.media_type_id,
        input.indexer_category_id,
    )
    .execute(exec)
    .await?;
    Ok(id)
}

pub async fn delete_category_mappings_by_source<'e, E>(
    exec: E,
    indexer_source_id: Uuid,
) -> Result<u64>
where
    E: Executor<'e, Database = Postgres>,
{
    let result = sqlx::query!(
        "DELETE FROM indexer_category_mappings WHERE indexer_source_id = $1",
        indexer_source_id
    )
    .execute(exec)
    .await?;
    Ok(result.rows_affected())
}
