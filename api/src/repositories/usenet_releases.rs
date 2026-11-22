use std::collections::HashMap;

use nats_jobs::NzbRef;
use sha2::{Digest, Sha256};
use sqlx::{Executor, Postgres};

use super::Result;

#[derive(Debug, Clone)]
pub struct Listing {
    pub indexer_id: String,
    pub guid: String,
    pub download_url: String,
    pub release_title: String,
}

impl Listing {
    pub fn nzb_ref(&self) -> NzbRef {
        nzb_ref(&self.indexer_id, &self.guid)
    }
}

pub fn nzb_ref(indexer_id: &str, guid: &str) -> NzbRef {
    NzbRef {
        indexer_id: indexer_id.to_string(),
        guid: const_hex::encode(Sha256::digest(format!("{indexer_id}|{guid}"))),
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Listed {
    pub release_key: Option<String>,
    pub failed_recently: bool,
}

#[derive(Debug, Clone)]
pub struct UsenetRelease {
    pub download_url: String,
    pub release_title: Option<String>,
    pub release_key: Option<String>,
    pub fetch_failure: Option<String>,
}

pub async fn upsert<'e, E>(exec: E, listings: &[Listing]) -> Result<HashMap<NzbRef, Listed>>
where
    E: Executor<'e, Database = Postgres>,
{
    if listings.is_empty() {
        return Ok(HashMap::new());
    }
    let refs: Vec<NzbRef> = listings.iter().map(Listing::nzb_ref).collect();
    let ref_keys: Vec<String> = refs.iter().map(|r| r.guid.clone()).collect();
    let indexer_ids: Vec<String> = listings.iter().map(|l| l.indexer_id.clone()).collect();
    let guids: Vec<String> = listings.iter().map(|l| l.guid.clone()).collect();
    let urls: Vec<String> = listings.iter().map(|l| l.download_url.clone()).collect();
    let titles: Vec<String> = listings.iter().map(|l| l.release_title.clone()).collect();
    let rows = sqlx::query!(
        r#"
        WITH upserted AS (
        INSERT INTO usenet_releases (indexer_id, guid, ref_key, download_url, release_title)
        SELECT DISTINCT ON (ref_key) indexer_id, guid, ref_key, download_url, release_title
        FROM UNNEST($1::text[], $2::text[], $3::text[], $4::text[], $5::text[])
            AS l(indexer_id, guid, ref_key, download_url, release_title)
        ON CONFLICT (indexer_id, guid) DO UPDATE SET
            download_url = EXCLUDED.download_url,
            release_title = EXCLUDED.release_title,
            updated_at = NOW()
        RETURNING indexer_id, ref_key, release_key, last_failure_at
        )
        SELECT indexer_id AS "indexer_id!", ref_key AS "ref_key!", release_key,
               COALESCE(last_failure_at > NOW() - INTERVAL '24 hours', false) AS "failed_recently!"
        FROM upserted
        "#,
        &indexer_ids,
        &guids,
        &ref_keys,
        &urls,
        &titles,
    )
    .fetch_all(exec)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| {
            (
                NzbRef {
                    indexer_id: r.indexer_id,
                    guid: r.ref_key,
                },
                Listed {
                    release_key: r.release_key,
                    failed_recently: r.failed_recently,
                },
            )
        })
        .collect())
}

pub async fn get<'e, E>(exec: E, nzb_ref: &NzbRef) -> Result<Option<UsenetRelease>>
where
    E: Executor<'e, Database = Postgres>,
{
    let row = sqlx::query_as!(
        UsenetRelease,
        r#"SELECT download_url, release_title, release_key,
                  CASE WHEN fetch_failed_until > NOW() THEN fetch_failure END AS fetch_failure
           FROM usenet_releases WHERE ref_key = $1 AND indexer_id = $2"#,
        nzb_ref.guid,
        nzb_ref.indexer_id,
    )
    .fetch_optional(exec)
    .await?;
    Ok(row)
}

pub async fn set_usenet_release_key<'e, E>(
    exec: E,
    nzb_ref: &NzbRef,
    release_key: &str,
) -> Result<bool>
where
    E: Executor<'e, Database = Postgres>,
{
    let result = sqlx::query!(
        r#"UPDATE usenet_releases
           SET release_key = $2, last_failure_at = NULL, last_failure = NULL,
               fetch_failed_until = NULL, fetch_failure = NULL, updated_at = NOW()
           WHERE ref_key = $1"#,
        nzb_ref.guid,
        release_key,
    )
    .execute(exec)
    .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn set_has_par2<'e, E>(exec: E, nzb_ref: &NzbRef, has_par2: bool) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query!(
        "UPDATE usenet_releases SET has_par2 = $2 WHERE ref_key = $1",
        nzb_ref.guid,
        has_par2,
    )
    .execute(exec)
    .await?;
    Ok(())
}

/// Any listing of the release whose NZB was seen with PAR2 files says the post has them.
pub async fn has_par2<'e, E>(exec: E, release_key: &str) -> Result<Option<bool>>
where
    E: Executor<'e, Database = Postgres>,
{
    let has_par2 = sqlx::query_scalar!(
        "SELECT bool_or(has_par2) FROM usenet_releases WHERE release_key = $1",
        release_key,
    )
    .fetch_one(exec)
    .await?;
    Ok(has_par2)
}

pub async fn aliases<'e, E>(exec: E, release_key: &str) -> Result<Vec<NzbRef>>
where
    E: Executor<'e, Database = Postgres>,
{
    let refs = sqlx::query_as!(
        NzbRef,
        r#"SELECT indexer_id, ref_key AS guid FROM usenet_releases WHERE release_key = $1
           ORDER BY indexer_id, ref_key"#,
        release_key,
    )
    .fetch_all(exec)
    .await?;
    Ok(refs)
}

pub async fn record_failure<'e, E>(exec: E, nzb_ref: &NzbRef, reason: &str) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query!(
        r#"UPDATE usenet_releases SET last_failure_at = NOW(), last_failure = $2, updated_at = NOW()
           WHERE ref_key = $1"#,
        nzb_ref.guid,
        reason,
    )
    .execute(exec)
    .await?;
    Ok(())
}

pub async fn record_fetch_failure<'e, E>(
    exec: E,
    nzb_ref: &NzbRef,
    reason: &str,
    minutes: i32,
) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query!(
        r#"UPDATE usenet_releases
           SET fetch_failed_until = NOW() + make_interval(mins => $3), fetch_failure = $2
           WHERE ref_key = $1"#,
        nzb_ref.guid,
        reason,
        minutes,
    )
    .execute(exec)
    .await?;
    Ok(())
}

pub async fn prune_unimported<'e, E>(exec: E, days: i32) -> Result<u64>
where
    E: Executor<'e, Database = Postgres>,
{
    let result = sqlx::query!(
        r#"DELETE FROM usenet_releases
           WHERE release_key IS NULL AND updated_at < NOW() - make_interval(days => $1)"#,
        days,
    )
    .execute(exec)
    .await?;
    Ok(result.rows_affected())
}
