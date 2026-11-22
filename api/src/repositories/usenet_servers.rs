use sqlx::{Executor, Postgres};
use time::{Date, OffsetDateTime};
use uuid::Uuid;

use super::Result;

#[derive(Clone)]
pub struct UsenetServerRow {
    pub id: Uuid,
    pub name: String,
    pub host: String,
    pub port: i32,
    pub tls: bool,
    pub verify_cert: bool,
    pub ip_preference: String,
    pub connections: i32,
    pub priority: i32,
    pub backbone: Option<String>,
    pub retention_days: Option<i32>,
    pub expires_on: Option<Date>,
    pub quota_bytes: Option<i64>,
    pub username: String,
    pub password: String,
    pub account_connection_cap: Option<i32>,
    pub enabled: bool,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Clone)]
pub struct UsenetServerInput {
    pub name: String,
    pub host: String,
    pub port: i32,
    pub tls: bool,
    pub verify_cert: bool,
    pub ip_preference: String,
    pub connections: i32,
    pub priority: i32,
    pub backbone: Option<String>,
    pub retention_days: Option<i32>,
    pub expires_on: Option<Date>,
    pub quota_bytes: Option<i64>,
    pub username: String,
    pub password: Option<String>,
    pub account_connection_cap: Option<i32>,
    pub enabled: bool,
}

pub async fn list<'e, E>(exec: E) -> Result<Vec<UsenetServerRow>>
where
    E: Executor<'e, Database = Postgres>,
{
    let rows = sqlx::query_as!(
        UsenetServerRow,
        r#"SELECT id, name, host, port, tls, verify_cert, ip_preference, connections, priority,
                  backbone, retention_days, expires_on, quota_bytes, username, password,
                  account_connection_cap, enabled, created_at, updated_at
           FROM usenet_servers
           ORDER BY priority, name"#
    )
    .fetch_all(exec)
    .await?;
    Ok(rows)
}

pub async fn get<'e, E>(exec: E, id: Uuid) -> Result<Option<UsenetServerRow>>
where
    E: Executor<'e, Database = Postgres>,
{
    let row = sqlx::query_as!(
        UsenetServerRow,
        r#"SELECT id, name, host, port, tls, verify_cert, ip_preference, connections, priority,
                  backbone, retention_days, expires_on, quota_bytes, username, password,
                  account_connection_cap, enabled, created_at, updated_at
           FROM usenet_servers
           WHERE id = $1"#,
        id,
    )
    .fetch_optional(exec)
    .await?;
    Ok(row)
}

pub async fn create<'e, E>(exec: E, input: &UsenetServerInput) -> Result<Uuid>
where
    E: Executor<'e, Database = Postgres>,
{
    let id = sqlx::query_scalar!(
        r#"INSERT INTO usenet_servers
               (name, host, port, tls, verify_cert, ip_preference, connections, priority, backbone,
                retention_days, expires_on, quota_bytes, username, password,
                account_connection_cap, enabled)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, COALESCE($14, ''), $15, $16)
           RETURNING id"#,
        input.name,
        input.host,
        input.port,
        input.tls,
        input.verify_cert,
        input.ip_preference,
        input.connections,
        input.priority,
        input.backbone,
        input.retention_days,
        input.expires_on,
        input.quota_bytes,
        input.username,
        input.password,
        input.account_connection_cap,
        input.enabled,
    )
    .fetch_one(exec)
    .await?;
    Ok(id)
}

pub async fn update<'e, E>(exec: E, id: Uuid, input: &UsenetServerInput) -> Result<bool>
where
    E: Executor<'e, Database = Postgres>,
{
    let result = sqlx::query!(
        r#"UPDATE usenet_servers SET
               name = $2, host = $3, port = $4, tls = $5, verify_cert = $6, ip_preference = $7,
               connections = $8, priority = $9, backbone = $10, retention_days = $11,
               expires_on = $12, quota_bytes = $13, username = $14,
               password = COALESCE($15, password), account_connection_cap = $16, enabled = $17,
               updated_at = NOW()
           WHERE id = $1"#,
        id,
        input.name,
        input.host,
        input.port,
        input.tls,
        input.verify_cert,
        input.ip_preference,
        input.connections,
        input.priority,
        input.backbone,
        input.retention_days,
        input.expires_on,
        input.quota_bytes,
        input.username,
        input.password,
        input.account_connection_cap,
        input.enabled,
    )
    .execute(exec)
    .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn delete<'e, E>(exec: E, id: Uuid) -> Result<bool>
where
    E: Executor<'e, Database = Postgres>,
{
    let result = sqlx::query!("DELETE FROM usenet_servers WHERE id = $1", id)
        .execute(exec)
        .await?;
    Ok(result.rows_affected() > 0)
}
