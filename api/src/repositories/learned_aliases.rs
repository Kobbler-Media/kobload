use sqlx::{Executor, Postgres};
use time::OffsetDateTime;
use uuid::Uuid;

use super::Result;

#[derive(Debug, Clone, serde::Serialize)]
pub struct LearnedAliasRow {
    pub id: Uuid,
    pub scope: String,
    pub scope_key: String,
    pub scope_label: String,
    pub alias: String,
    #[serde(with = "time::serde::rfc3339")]
    pub created_at: OffsetDateTime,
}

pub struct NewLearnedAlias<'a> {
    pub scope: &'a str,
    pub scope_key: &'a str,
    pub scope_label: &'a str,
    pub alias: &'a str,
}

pub async fn insert<'e, E>(exec: E, alias: &NewLearnedAlias<'_>) -> Result<LearnedAliasRow>
where
    E: Executor<'e, Database = Postgres>,
{
    let row = sqlx::query_as!(
        LearnedAliasRow,
        r#"
        INSERT INTO learned_aliases (scope, scope_key, scope_label, alias)
        VALUES ($1, $2, $3, btrim($4))
        ON CONFLICT (scope, scope_key, lower(alias)) DO UPDATE SET scope_label = EXCLUDED.scope_label
        RETURNING id, scope, scope_key, scope_label, alias, created_at
        "#,
        alias.scope,
        alias.scope_key,
        alias.scope_label,
        alias.alias,
    )
    .fetch_one(exec)
    .await?;
    Ok(row)
}

pub async fn delete<'e, E>(exec: E, id: Uuid) -> Result<bool>
where
    E: Executor<'e, Database = Postgres>,
{
    let result = sqlx::query!("DELETE FROM learned_aliases WHERE id = $1", id)
        .execute(exec)
        .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn list<'e, E>(exec: E) -> Result<Vec<LearnedAliasRow>>
where
    E: Executor<'e, Database = Postgres>,
{
    let rows = sqlx::query_as!(
        LearnedAliasRow,
        r#"
        SELECT id, scope, scope_key, scope_label, alias, created_at
        FROM learned_aliases
        ORDER BY scope_label, alias
        "#
    )
    .fetch_all(exec)
    .await?;
    Ok(rows)
}

pub async fn for_keys<'e, E>(exec: E, keys: &[String]) -> Result<Vec<LearnedAliasRow>>
where
    E: Executor<'e, Database = Postgres>,
{
    let rows = sqlx::query_as!(
        LearnedAliasRow,
        r#"
        SELECT id, scope, scope_key, scope_label, alias, created_at
        FROM learned_aliases
        WHERE scope_key = ANY($1)
        ORDER BY created_at
        "#,
        keys,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows)
}
