//! Repository for handler_type_defaults table.

use sqlx::{Executor, Postgres};
use uuid::Uuid;

use super::Result;

#[derive(Debug, Clone)]
pub struct HandlerTypeDefaultRow {
    pub id: Uuid,
    pub handler_type: String,
    pub media_type_id: Uuid,
    pub category_ids: Vec<String>,
}

pub async fn get_categories<'e, E>(
    exec: E,
    handler_type: &str,
    media_type_id: Uuid,
) -> Result<Option<Vec<String>>>
where
    E: Executor<'e, Database = Postgres>,
{
    let row = sqlx::query_scalar!(
        "SELECT category_ids FROM handler_type_defaults WHERE handler_type = $1 AND media_type_id = $2",
        handler_type,
        media_type_id,
    )
    .fetch_optional(exec)
    .await?;
    Ok(row)
}

pub async fn list_all<'e, E>(exec: E) -> Result<Vec<HandlerTypeDefaultRow>>
where
    E: Executor<'e, Database = Postgres>,
{
    let rows = sqlx::query_as!(
        HandlerTypeDefaultRow,
        "SELECT id, handler_type, media_type_id, category_ids FROM handler_type_defaults ORDER BY handler_type, media_type_id",
    )
    .fetch_all(exec)
    .await?;
    Ok(rows)
}

pub async fn upsert<'e, E>(
    exec: E,
    handler_type: &str,
    media_type_id: Uuid,
    category_ids: &[String],
) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query!(
        r#"
        INSERT INTO handler_type_defaults (id, handler_type, media_type_id, category_ids)
        VALUES (gen_random_uuid(), $1, $2, $3)
        ON CONFLICT (handler_type, media_type_id) DO UPDATE SET category_ids = EXCLUDED.category_ids
        "#,
        handler_type,
        media_type_id,
        category_ids,
    )
    .execute(exec)
    .await?;
    Ok(())
}
