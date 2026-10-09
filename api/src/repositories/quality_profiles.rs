//! Quality profile repository.

use super::Result;
use serde::Serialize;
use sqlx::{Executor, Postgres};
use time::OffsetDateTime;
use uuid::Uuid;

#[derive(Debug, Clone, Serialize)]
pub struct QualityProfileRow {
    pub id: Uuid,
    pub name: String,
    pub media_kind: String,
    pub min_resolution: Option<String>,
    pub max_resolution: Option<String>,
    pub preferred_sources: Vec<String>,
    pub preferred_codecs: Vec<String>,
    pub preferred_audio: Vec<String>,
    pub preferred_languages: Vec<String>,
    pub rejected_sources: Vec<String>,
    pub rejected_languages: Vec<String>,
    pub rejected_terms: Vec<String>,
    pub reject_hardcoded_subs: bool,
    pub reject_remux: bool,
    pub use_trash_filters: bool,
    pub is_default: bool,
    pub upgrade_cutoff_score: Option<f64>,
    pub created_at: OffsetDateTime,
    pub updated_at: OffsetDateTime,
}

#[derive(Debug, Default)]
pub struct QualityProfileInput {
    pub name: String,
    pub media_kind: String,
    pub min_resolution: Option<String>,
    pub max_resolution: Option<String>,
    pub preferred_sources: Vec<String>,
    pub preferred_codecs: Vec<String>,
    pub preferred_audio: Vec<String>,
    pub preferred_languages: Vec<String>,
    pub rejected_sources: Vec<String>,
    pub rejected_languages: Vec<String>,
    pub rejected_terms: Vec<String>,
    pub reject_hardcoded_subs: bool,
    pub reject_remux: bool,
    pub use_trash_filters: bool,
    pub is_default: bool,
    pub upgrade_cutoff_score: Option<f64>,
}

#[derive(Debug, Default)]
pub struct QualityProfileUpdate {
    pub name: Option<String>,
    pub media_kind: Option<String>,
    pub min_resolution: Option<Option<String>>,
    pub max_resolution: Option<Option<String>>,
    pub preferred_sources: Option<Vec<String>>,
    pub preferred_codecs: Option<Vec<String>>,
    pub preferred_audio: Option<Vec<String>>,
    pub preferred_languages: Option<Vec<String>>,
    pub rejected_sources: Option<Vec<String>>,
    pub rejected_languages: Option<Vec<String>>,
    pub rejected_terms: Option<Vec<String>>,
    pub reject_hardcoded_subs: Option<bool>,
    pub reject_remux: Option<bool>,
    pub use_trash_filters: Option<bool>,
    pub is_default: Option<bool>,
    pub upgrade_cutoff_score: Option<Option<f64>>,
}

pub async fn list_quality_profiles<'e, E>(exec: E) -> Result<Vec<QualityProfileRow>>
where
    E: Executor<'e, Database = Postgres>,
{
    let rows = sqlx::query_as!(
        QualityProfileRow,
        r#"SELECT
            id, name, media_kind, min_resolution, max_resolution,
            preferred_sources, preferred_codecs, preferred_audio,
            preferred_languages, rejected_sources, rejected_languages, rejected_terms,
            reject_hardcoded_subs, reject_remux, use_trash_filters, is_default, upgrade_cutoff_score,
            created_at, updated_at
        FROM quality_profiles ORDER BY created_at DESC"#
    )
    .fetch_all(exec)
    .await?;
    Ok(rows)
}

pub async fn quality_profile_count<'e, E>(exec: E) -> Result<i64>
where
    E: Executor<'e, Database = Postgres>,
{
    let count = sqlx::query_scalar!(r#"SELECT COUNT(*) as "count!" FROM quality_profiles"#)
        .fetch_one(exec)
        .await?;
    Ok(count)
}

pub async fn get_quality_profile_by_id<'e, E>(
    exec: E,
    id: Uuid,
) -> Result<Option<QualityProfileRow>>
where
    E: Executor<'e, Database = Postgres>,
{
    let row = sqlx::query_as!(
        QualityProfileRow,
        r#"SELECT
            id, name, media_kind, min_resolution, max_resolution,
            preferred_sources, preferred_codecs, preferred_audio,
            preferred_languages, rejected_sources, rejected_languages, rejected_terms,
            reject_hardcoded_subs, reject_remux, use_trash_filters, is_default, upgrade_cutoff_score,
            created_at, updated_at
        FROM quality_profiles WHERE id = $1"#,
        id
    )
    .fetch_optional(exec)
    .await?;
    Ok(row)
}

pub async fn create_quality_profile<'e, E>(exec: E, input: QualityProfileInput) -> Result<Uuid>
where
    E: Executor<'e, Database = Postgres>,
{
    let id = Uuid::now_v7();
    sqlx::query!(
        r#"
        INSERT INTO quality_profiles (
            id, name, media_kind, min_resolution, max_resolution,
            preferred_sources, preferred_codecs, preferred_audio,
            preferred_languages, rejected_sources, rejected_languages, rejected_terms,
            reject_hardcoded_subs, reject_remux, use_trash_filters, is_default, upgrade_cutoff_score,
            created_at, updated_at
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, NOW(), NOW())
        "#,
        id,
        &input.name,
        &input.media_kind,
        input.min_resolution.as_deref(),
        input.max_resolution.as_deref(),
        &input.preferred_sources,
        &input.preferred_codecs,
        &input.preferred_audio,
        &input.preferred_languages,
        &input.rejected_sources,
        &input.rejected_languages,
        &input.rejected_terms,
        input.reject_hardcoded_subs,
        input.reject_remux,
        input.use_trash_filters,
        input.is_default,
        input.upgrade_cutoff_score,
    )
    .execute(exec)
    .await?;
    Ok(id)
}

pub async fn update_quality_profile<'e, E>(
    exec: E,
    id: Uuid,
    input: QualityProfileUpdate,
) -> Result<bool>
where
    E: Executor<'e, Database = Postgres>,
{
    let min_resolution = input.min_resolution.flatten();
    let max_resolution = input.max_resolution.flatten();
    let result = sqlx::query!(
        r#"
        UPDATE quality_profiles SET
            name = COALESCE($2, name),
            media_kind = COALESCE($3, media_kind),
            min_resolution = COALESCE($4, min_resolution),
            max_resolution = COALESCE($5, max_resolution),
            preferred_sources = COALESCE($6, preferred_sources),
            preferred_codecs = COALESCE($7, preferred_codecs),
            preferred_audio = COALESCE($8, preferred_audio),
            preferred_languages = COALESCE($9, preferred_languages),
            rejected_sources = COALESCE($10, rejected_sources),
            rejected_languages = COALESCE($11, rejected_languages),
            rejected_terms = COALESCE($12, rejected_terms),
            reject_hardcoded_subs = COALESCE($13, reject_hardcoded_subs),
            reject_remux = COALESCE($14, reject_remux),
            use_trash_filters = COALESCE($15, use_trash_filters),
            is_default = COALESCE($16, is_default),
            upgrade_cutoff_score = COALESCE($17, upgrade_cutoff_score),
            updated_at = NOW()
        WHERE id = $1
        "#,
        id,
        input.name.as_deref(),
        input.media_kind.as_deref(),
        min_resolution.as_deref(),
        max_resolution.as_deref(),
        input.preferred_sources.as_deref(),
        input.preferred_codecs.as_deref(),
        input.preferred_audio.as_deref(),
        input.preferred_languages.as_deref(),
        input.rejected_sources.as_deref(),
        input.rejected_languages.as_deref(),
        input.rejected_terms.as_deref(),
        input.reject_hardcoded_subs as Option<bool>,
        input.reject_remux as Option<bool>,
        input.use_trash_filters as Option<bool>,
        input.is_default as Option<bool>,
        input.upgrade_cutoff_score.flatten(),
    )
    .execute(exec)
    .await?;

    Ok(result.rows_affected() > 0)
}

pub async fn delete_quality_profile<'e, E>(exec: E, id: Uuid) -> Result<bool>
where
    E: Executor<'e, Database = Postgres>,
{
    let result = sqlx::query!("DELETE FROM quality_profiles WHERE id = $1", id)
        .execute(exec)
        .await?;
    Ok(result.rows_affected() > 0)
}

/// Upsert a quality profile by (name, media_kind). Returns the profile ID.
/// If a profile with the same name+media_kind exists, updates it in place (preserving ID).
/// Otherwise inserts a new row.
pub async fn upsert_quality_profile<'e, E>(exec: E, input: QualityProfileInput) -> Result<Uuid>
where
    E: Executor<'e, Database = Postgres>,
{
    let id = Uuid::now_v7();
    let row = sqlx::query_scalar!(
        r#"
        INSERT INTO quality_profiles (
            id, name, media_kind, min_resolution, max_resolution,
            preferred_sources, preferred_codecs, preferred_audio,
            preferred_languages, rejected_sources, rejected_languages, rejected_terms,
            reject_hardcoded_subs, reject_remux, use_trash_filters, is_default, upgrade_cutoff_score,
            created_at, updated_at
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, NOW(), NOW())
        ON CONFLICT (name, media_kind) DO UPDATE SET
            min_resolution = EXCLUDED.min_resolution,
            max_resolution = EXCLUDED.max_resolution,
            preferred_sources = EXCLUDED.preferred_sources,
            preferred_codecs = EXCLUDED.preferred_codecs,
            preferred_audio = EXCLUDED.preferred_audio,
            preferred_languages = EXCLUDED.preferred_languages,
            rejected_sources = EXCLUDED.rejected_sources,
            rejected_languages = EXCLUDED.rejected_languages,
            rejected_terms = EXCLUDED.rejected_terms,
            reject_hardcoded_subs = EXCLUDED.reject_hardcoded_subs,
            reject_remux = EXCLUDED.reject_remux,
            use_trash_filters = EXCLUDED.use_trash_filters,
            is_default = EXCLUDED.is_default,
            upgrade_cutoff_score = EXCLUDED.upgrade_cutoff_score,
            updated_at = NOW()
        RETURNING id
        "#,
        id,
        &input.name,
        &input.media_kind,
        input.min_resolution.as_deref(),
        input.max_resolution.as_deref(),
        &input.preferred_sources,
        &input.preferred_codecs,
        &input.preferred_audio,
        &input.preferred_languages,
        &input.rejected_sources,
        &input.rejected_languages,
        &input.rejected_terms,
        input.reject_hardcoded_subs,
        input.reject_remux,
        input.use_trash_filters,
        input.is_default,
        input.upgrade_cutoff_score,
    )
    .fetch_one(exec)
    .await?;
    Ok(row)
}

/// Delete all quality profiles whose IDs are NOT in the given list.
pub async fn delete_quality_profiles_not_in<'e, E>(exec: E, keep_ids: &[Uuid]) -> Result<u64>
where
    E: Executor<'e, Database = Postgres>,
{
    let result = sqlx::query!("DELETE FROM quality_profiles WHERE id != ALL($1)", keep_ids)
        .execute(exec)
        .await?;
    Ok(result.rows_affected())
}

/// Get the default quality profile for a given media_kind.
pub async fn get_default_for_media_kind<'e, E>(
    exec: E,
    media_kind: &str,
) -> Result<Option<QualityProfileRow>>
where
    E: Executor<'e, Database = Postgres>,
{
    let row = sqlx::query_as!(
        QualityProfileRow,
        r#"SELECT
            id, name, media_kind, min_resolution, max_resolution,
            preferred_sources, preferred_codecs, preferred_audio,
            preferred_languages, rejected_sources, rejected_languages, rejected_terms,
            reject_hardcoded_subs, reject_remux, use_trash_filters, is_default, upgrade_cutoff_score,
            created_at, updated_at
        FROM quality_profiles WHERE media_kind = $1 AND is_default = TRUE"#,
        media_kind
    )
    .fetch_optional(exec)
    .await?;
    Ok(row)
}

/// Clear the is_default flag on all profiles of a given media_kind except the specified one.
pub async fn clear_defaults_for_media_kind_except<'e, E>(
    exec: E,
    media_kind: &str,
    except_id: Uuid,
) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query!(
        "UPDATE quality_profiles SET is_default = FALSE WHERE media_kind = $1 AND id != $2 AND is_default = TRUE",
        media_kind,
        except_id,
    )
    .execute(exec)
    .await?;
    Ok(())
}
