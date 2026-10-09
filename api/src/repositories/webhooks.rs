use sqlx::{Executor, Postgres};
use time::OffsetDateTime;
use uuid::Uuid;

use super::Result;

#[derive(Debug, Clone)]
pub struct StatusWebhook {
    pub id: Uuid,
    pub url: String,
    pub secret: Option<String>,
    pub events: Vec<String>,
    pub enabled: bool,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Default)]
pub struct WebhookInput {
    pub url: String,
    pub secret: Option<String>,
    pub events: Vec<String>,
}

#[derive(Debug, Default)]
pub struct WebhookUpdate {
    pub url: Option<String>,
    pub secret: Option<Option<String>>,
    pub events: Option<Vec<String>>,
    pub enabled: Option<bool>,
}

pub async fn create<'e, E>(exec: E, input: WebhookInput) -> Result<Uuid>
where
    E: Executor<'e, Database = Postgres>,
{
    let id = Uuid::now_v7();
    sqlx::query!(
        r#"
        INSERT INTO status_webhooks (id, url, secret, events)
        VALUES ($1, $2, $3, $4)
        "#,
        id,
        &input.url,
        input.secret as Option<String>,
        &input.events,
    )
    .execute(exec)
    .await?;
    Ok(id)
}

pub async fn list<'e, E>(exec: E) -> Result<Vec<StatusWebhook>>
where
    E: Executor<'e, Database = Postgres>,
{
    let rows = sqlx::query_as!(
        StatusWebhook,
        "SELECT id, url, secret, events, enabled, created_at, updated_at FROM status_webhooks ORDER BY created_at"
    )
    .fetch_all(exec)
    .await?;

    Ok(rows)
}

pub async fn list_enabled_for_event<'e, E>(exec: E, event: &str) -> Result<Vec<StatusWebhook>>
where
    E: Executor<'e, Database = Postgres>,
{
    let rows = sqlx::query_as!(
        StatusWebhook,
        r#"
        SELECT id, url, secret, events, enabled, created_at, updated_at
        FROM status_webhooks
        WHERE enabled = true AND (events = '{}' OR $1 = ANY(events))
        ORDER BY created_at
        "#,
        event
    )
    .fetch_all(exec)
    .await?;

    Ok(rows)
}

pub async fn update<'e, E>(exec: E, id: Uuid, input: WebhookUpdate) -> Result<bool>
where
    E: Executor<'e, Database = Postgres>,
{
    let secret_provided = input.secret.is_some();
    let secret_value = input.secret.flatten();
    let result = sqlx::query!(
        r#"
        UPDATE status_webhooks SET
            url = COALESCE($2, url),
            secret = CASE WHEN $3 THEN $4 ELSE secret END,
            events = COALESCE($5, events),
            enabled = COALESCE($6, enabled),
            updated_at = NOW()
        WHERE id = $1
        "#,
        id,
        input.url as Option<String>,
        secret_provided,
        secret_value as Option<String>,
        input.events.as_deref() as Option<&[String]>,
        input.enabled as Option<bool>,
    )
    .execute(exec)
    .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn delete<'e, E>(exec: E, id: Uuid) -> Result<bool>
where
    E: Executor<'e, Database = Postgres>,
{
    let result = sqlx::query!("DELETE FROM status_webhooks WHERE id = $1", id)
        .execute(exec)
        .await?;
    Ok(result.rows_affected() > 0)
}
