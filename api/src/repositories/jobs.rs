//! Jobs repository: job_status tracking for UI visibility.

use super::Result;
use serde_json::Value;
use sqlx::{Executor, Postgres};
use uuid::Uuid;

/// Insert initial job status for UI visibility.
/// Uses ON CONFLICT DO NOTHING since the job may already exist.
pub async fn insert_job_status<'e, E>(
    exec: E,
    job_id: Uuid,
    job_type: &str,
    payload: &Value,
    entity_type: Option<&str>,
    entity_id: Option<Uuid>,
    description: &str,
) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query!(
        r#"
        INSERT INTO job_status (id, job_type, status, payload, entity_type, entity_id, description, created_at)
        VALUES ($1, $2, 'pending', $3, $4, $5, $6, NOW())
        ON CONFLICT (id) DO NOTHING
        "#,
        job_id,
        job_type,
        payload,
        entity_type as Option<&str>,
        entity_id as Option<Uuid>,
        description,
    )
    .execute(exec)
    .await?;
    Ok(())
}
