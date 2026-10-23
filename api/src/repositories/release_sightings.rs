use sqlx::{Executor, Postgres};
use time::OffsetDateTime;
use uuid::Uuid;

use super::Result;
use crate::watch::Release;

pub const RETENTION_DAYS: i32 = 14;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Sighting {
    pub release_key: String,
    pub title: String,
    pub size: Option<i64>,
    #[serde(with = "time::serde::rfc3339::option")]
    pub published_at: Option<OffsetDateTime>,
    pub indexer_source_id: Option<Uuid>,
    pub indexer: String,
    pub protocol: String,
    pub download_url: Option<String>,
    pub magnet_url: Option<String>,
    pub info_hash: Option<String>,
}

impl Sighting {
    pub fn of(release: &Release, indexer_source_id: Uuid) -> Self {
        let info_hash = release
            .info_hash
            .as_deref()
            .filter(|h| !h.is_empty())
            .map(str::to_lowercase);
        Self {
            release_key: info_hash.clone().unwrap_or_else(|| release.guid.clone()),
            title: release.title.clone(),
            size: (release.size > 0).then_some(release.size),
            published_at: crate::services::upgrade::parse_publish_date(&release.publish_date),
            indexer_source_id: Some(indexer_source_id),
            indexer: release.indexer.clone(),
            protocol: release.protocol.clone(),
            download_url: release.download_url.clone(),
            magnet_url: release.magnet_url.clone(),
            info_hash,
        }
    }
}

pub async fn record<'e, E>(exec: E, sightings: &[Sighting]) -> Result<u64>
where
    E: Executor<'e, Database = Postgres>,
{
    if sightings.is_empty() {
        return Ok(0);
    }
    let col = |f: fn(&Sighting) -> Option<String>| sightings.iter().map(f).collect::<Vec<_>>();
    let keys: Vec<String> = sightings.iter().map(|s| s.release_key.clone()).collect();
    let titles: Vec<String> = sightings.iter().map(|s| s.title.clone()).collect();
    let sizes: Vec<Option<i64>> = sightings.iter().map(|s| s.size).collect();
    let published: Vec<Option<OffsetDateTime>> = sightings.iter().map(|s| s.published_at).collect();
    let sources: Vec<Option<Uuid>> = sightings.iter().map(|s| s.indexer_source_id).collect();
    let indexers: Vec<String> = sightings.iter().map(|s| s.indexer.clone()).collect();
    let protocols: Vec<String> = sightings.iter().map(|s| s.protocol.clone()).collect();
    let download_urls = col(|s| s.download_url.clone());
    let magnet_urls = col(|s| s.magnet_url.clone());
    let info_hashes = col(|s| s.info_hash.clone());
    let result = sqlx::query!(
        r#"
        INSERT INTO release_sightings
            (release_key, title, size, published_at, indexer_source_id, indexer, protocol,
             download_url, magnet_url, info_hash)
        SELECT * FROM UNNEST(
            $1::text[], $2::text[], $3::bigint[], $4::timestamptz[], $5::uuid[], $6::text[],
            $7::text[], $8::text[], $9::text[], $10::text[]
        )
        ON CONFLICT (release_key) DO NOTHING
        "#,
        &keys,
        &titles,
        &sizes as &[Option<i64>],
        &published as &[Option<OffsetDateTime>],
        &sources as &[Option<Uuid>],
        &indexers,
        &protocols,
        &download_urls as &[Option<String>],
        &magnet_urls as &[Option<String>],
        &info_hashes as &[Option<String>],
    )
    .execute(exec)
    .await?;
    Ok(result.rows_affected())
}

pub async fn prune<'e, E>(exec: E) -> Result<u64>
where
    E: Executor<'e, Database = Postgres>,
{
    let result = sqlx::query!(
        "DELETE FROM release_sightings WHERE seen_at < NOW() - make_interval(days => $1)",
        RETENTION_DAYS,
    )
    .execute(exec)
    .await?;
    Ok(result.rows_affected())
}

/// Sightings since `since` whose title has any of `words`, for the review
/// queue. `words` must be plain alphanumeric terms.
pub async fn seen_with_any_word<'e, E>(
    exec: E,
    since: OffsetDateTime,
    words: &[String],
) -> Result<Vec<Sighting>>
where
    E: Executor<'e, Database = Postgres>,
{
    let patterns: Vec<String> = words.iter().map(|w| format!("%{w}%")).collect();
    let rows = sqlx::query_as!(
        Sighting,
        r#"
        SELECT release_key, title, size, published_at, indexer_source_id, indexer, protocol,
               download_url, magnet_url, info_hash
        FROM release_sightings
        WHERE seen_at >= $1 AND title ILIKE ANY($2)
        "#,
        since,
        &patterns,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows)
}
