//! Repository for media_type_config table.

use sqlx::{Executor, Postgres};
use uuid::Uuid;

use super::Result;

#[derive(Debug, Clone)]
pub struct MediaTypeConfigRow {
    pub id: Uuid,
    pub key: String,
    pub name: String,
    pub protocol_priority: Vec<String>,
    pub protocol_delay_minutes: Option<i32>,
    pub created_at: time::OffsetDateTime,
    pub updated_at: time::OffsetDateTime,
}

pub async fn get_by_key<'e, E>(exec: E, key: &str) -> Result<Option<MediaTypeConfigRow>>
where
    E: Executor<'e, Database = Postgres>,
{
    let row = sqlx::query_as!(
        MediaTypeConfigRow,
        "SELECT id, key, name, protocol_priority, protocol_delay_minutes, created_at, updated_at FROM media_type_config WHERE key = $1",
        key,
    )
    .fetch_optional(exec)
    .await?;
    Ok(row)
}

pub async fn get_by_id<'e, E>(exec: E, id: Uuid) -> Result<Option<MediaTypeConfigRow>>
where
    E: Executor<'e, Database = Postgres>,
{
    let row = sqlx::query_as!(
        MediaTypeConfigRow,
        "SELECT id, key, name, protocol_priority, protocol_delay_minutes, created_at, updated_at FROM media_type_config WHERE id = $1",
        id,
    )
    .fetch_optional(exec)
    .await?;
    Ok(row)
}

pub async fn list_all<'e, E>(exec: E) -> Result<Vec<MediaTypeConfigRow>>
where
    E: Executor<'e, Database = Postgres>,
{
    let rows = sqlx::query_as!(
        MediaTypeConfigRow,
        "SELECT id, key, name, protocol_priority, protocol_delay_minutes, created_at, updated_at FROM media_type_config ORDER BY key",
    )
    .fetch_all(exec)
    .await?;
    Ok(rows)
}

pub async fn upsert<'e, E>(exec: E, key: &str, name: &str) -> Result<Uuid>
where
    E: Executor<'e, Database = Postgres>,
{
    let id = Uuid::now_v7();
    let row = sqlx::query_scalar!(
        r#"
        INSERT INTO media_type_config (id, key, name)
        VALUES ($1, $2, $3)
        ON CONFLICT (key) DO UPDATE SET name = EXCLUDED.name, updated_at = NOW()
        RETURNING id
        "#,
        id,
        key,
        name,
    )
    .fetch_one(exec)
    .await?;
    Ok(row)
}

pub async fn insert_default<'e, E>(exec: E, key: &str, name: &str) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query!(
        r#"
        INSERT INTO media_type_config (id, key, name)
        VALUES ($1, $2, $3)
        ON CONFLICT (key) DO NOTHING
        "#,
        Uuid::now_v7(),
        key,
        name,
    )
    .execute(exec)
    .await?;
    Ok(())
}

#[derive(Debug, Default)]
pub struct MediaTypeConfigUpdate {
    pub name: Option<String>,
    pub protocol_priority: Option<Vec<String>>,
    pub protocol_delay_minutes: Option<Option<i32>>,
}

pub async fn update<'e, E>(exec: E, id: Uuid, input: MediaTypeConfigUpdate) -> Result<bool>
where
    E: Executor<'e, Database = Postgres>,
{
    let result = sqlx::query!(
        r#"
        UPDATE media_type_config SET
            name = COALESCE($2, name),
            protocol_priority = COALESCE($3, protocol_priority),
            protocol_delay_minutes = COALESCE($4, protocol_delay_minutes),
            updated_at = NOW()
        WHERE id = $1
        "#,
        id,
        input.name as Option<String>,
        input.protocol_priority.as_deref() as Option<&[String]>,
        input.protocol_delay_minutes.flatten() as Option<i32>,
    )
    .execute(exec)
    .await?;
    Ok(result.rows_affected() > 0)
}
