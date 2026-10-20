use sqlx::{Executor, Postgres};
use time::OffsetDateTime;
use uuid::Uuid;

use super::Result;

#[derive(Debug, Clone)]
pub struct ReleaseOutcomeRow {
    pub info_hash: String,
    pub file_id: Option<i32>,
    pub last_success_at: Option<OffsetDateTime>,
    pub last_failure_at: Option<OffsetDateTime>,
    pub binge_group: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TraitRow {
    pub trait_name: String,
    pub success_weight: f64,
    pub failure_weight: f64,
}

pub struct TraitDecay {
    pub half_life_secs: f64,
    pub cap: f64,
}

pub async fn record_success<'e, E>(
    exec: E,
    provider_id: Uuid,
    info_hash: &str,
    file_id: Option<i32>,
    binge_group: Option<&str>,
) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query!(
        r#"
        INSERT INTO stream_release_outcomes
            (provider_id, info_hash, file_id, successes, last_success_at, binge_group)
        VALUES ($1, $2, $3, 1, NOW(), $4)
        ON CONFLICT (provider_id, info_hash, file_id) DO UPDATE SET
            successes = stream_release_outcomes.successes + 1,
            last_success_at = NOW(),
            binge_group = COALESCE(EXCLUDED.binge_group, stream_release_outcomes.binge_group),
            updated_at = NOW()
        "#,
        provider_id,
        info_hash,
        file_id,
        binge_group,
    )
    .execute(exec)
    .await?;
    Ok(())
}

pub async fn record_failure<'e, E>(
    exec: E,
    provider_id: Uuid,
    info_hash: &str,
    file_id: Option<i32>,
    reason: &str,
) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query!(
        r#"
        INSERT INTO stream_release_outcomes
            (provider_id, info_hash, file_id, failures, last_failure_at, last_failure_reason)
        VALUES ($1, $2, $3, 1, NOW(), $4)
        ON CONFLICT (provider_id, info_hash, file_id) DO UPDATE SET
            failures = stream_release_outcomes.failures + 1,
            last_failure_at = NOW(),
            last_failure_reason = EXCLUDED.last_failure_reason,
            updated_at = NOW()
        "#,
        provider_id,
        info_hash,
        file_id,
        reason,
    )
    .execute(exec)
    .await?;
    Ok(())
}

pub async fn releases_for<'e, E>(
    exec: E,
    provider_id: Uuid,
    info_hashes: &[String],
) -> Result<Vec<ReleaseOutcomeRow>>
where
    E: Executor<'e, Database = Postgres>,
{
    let rows = sqlx::query_as!(
        ReleaseOutcomeRow,
        r#"
        SELECT info_hash, file_id, last_success_at, last_failure_at, binge_group
        FROM stream_release_outcomes
        WHERE provider_id = $1 AND info_hash = ANY($2)
        "#,
        provider_id,
        info_hashes,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows)
}

pub async fn add_trait_evidence<'e, E>(
    exec: E,
    provider_id: Uuid,
    traits: &[String],
    success: f64,
    failure: f64,
    decay: &TraitDecay,
) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query!(
        r#"
        INSERT INTO stream_trait_outcomes (provider_id, trait, success_weight, failure_weight)
        SELECT $1, t, LEAST($3::float8, $5::float8), LEAST($4::float8, $5::float8)
        FROM UNNEST($2::text[]) AS t
        ON CONFLICT (provider_id, trait) DO UPDATE SET
            success_weight = LEAST($5, stream_trait_outcomes.success_weight
                * POWER(0.5, EXTRACT(EPOCH FROM NOW() - stream_trait_outcomes.updated_at)::float8 / $6::float8)
                + EXCLUDED.success_weight),
            failure_weight = LEAST($5, stream_trait_outcomes.failure_weight
                * POWER(0.5, EXTRACT(EPOCH FROM NOW() - stream_trait_outcomes.updated_at)::float8 / $6::float8)
                + EXCLUDED.failure_weight),
            updated_at = NOW()
        "#,
        provider_id,
        traits,
        success,
        failure,
        decay.cap,
        decay.half_life_secs,
    )
    .execute(exec)
    .await?;
    Ok(())
}

pub async fn traits_for<'e, E>(
    exec: E,
    provider_id: Uuid,
    traits: &[String],
    half_life_secs: f64,
) -> Result<Vec<TraitRow>>
where
    E: Executor<'e, Database = Postgres>,
{
    let rows = sqlx::query_as!(
        TraitRow,
        r#"
        SELECT trait AS trait_name,
               (success_weight * POWER(0.5, EXTRACT(EPOCH FROM NOW() - updated_at)::float8 / $3))
                   AS "success_weight!",
               (failure_weight * POWER(0.5, EXTRACT(EPOCH FROM NOW() - updated_at)::float8 / $3))
                   AS "failure_weight!"
        FROM stream_trait_outcomes
        WHERE provider_id = $1 AND trait = ANY($2)
        "#,
        provider_id,
        traits,
        half_life_secs,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows)
}

pub async fn prune_releases<'e, E>(
    exec: E,
    failure_memory_hours: i32,
    success_memory_days: i32,
) -> Result<u64>
where
    E: Executor<'e, Database = Postgres>,
{
    let result = sqlx::query!(
        r#"
        DELETE FROM stream_release_outcomes
        WHERE (last_failure_at IS NULL
               OR last_failure_at < NOW() - make_interval(hours => $1))
          AND (last_success_at IS NULL
               OR last_success_at < NOW() - make_interval(days => $2))
        "#,
        failure_memory_hours,
        success_memory_days,
    )
    .execute(exec)
    .await?;
    Ok(result.rows_affected())
}

pub async fn prune_traits<'e, E>(exec: E, half_life_secs: f64, min_weight: f64) -> Result<u64>
where
    E: Executor<'e, Database = Postgres>,
{
    let result = sqlx::query!(
        r#"
        DELETE FROM stream_trait_outcomes
        WHERE (success_weight + failure_weight)
              * POWER(0.5, EXTRACT(EPOCH FROM NOW() - updated_at)::float8 / $1) < $2
        "#,
        half_life_secs,
        min_weight,
    )
    .execute(exec)
    .await?;
    Ok(result.rows_affected())
}

pub async fn default_provider<'e, E>(exec: E) -> Result<Option<Uuid>>
where
    E: Executor<'e, Database = Postgres>,
{
    let id = sqlx::query_scalar!(
        "SELECT id FROM debrid_providers WHERE enabled = true ORDER BY priority DESC, name LIMIT 1"
    )
    .fetch_optional(exec)
    .await?;
    Ok(id)
}
