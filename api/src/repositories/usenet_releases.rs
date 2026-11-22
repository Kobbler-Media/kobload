use std::collections::HashMap;

use nats_jobs::NzbRef;
use sqlx::{Executor, Postgres};

use super::Result;

#[derive(Debug, Clone)]
pub struct Listing {
    pub nzb_ref: NzbRef,
    pub download_url: String,
    pub release_title: String,
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
}

pub async fn upsert<'e, E>(exec: E, listings: &[Listing]) -> Result<HashMap<NzbRef, Listed>>
where
    E: Executor<'e, Database = Postgres>,
{
    if listings.is_empty() {
        return Ok(HashMap::new());
    }
    let indexer_ids: Vec<String> = listings
        .iter()
        .map(|l| l.nzb_ref.indexer_id.clone())
        .collect();
    let guids: Vec<String> = listings.iter().map(|l| l.nzb_ref.guid.clone()).collect();
    let urls: Vec<String> = listings.iter().map(|l| l.download_url.clone()).collect();
    let titles: Vec<String> = listings.iter().map(|l| l.release_title.clone()).collect();
    let rows = sqlx::query!(
        r#"
        WITH upserted AS (
        INSERT INTO usenet_releases (indexer_id, guid, download_url, release_title)
        SELECT DISTINCT ON (indexer_id, guid) indexer_id, guid, download_url, release_title
        FROM UNNEST($1::text[], $2::text[], $3::text[], $4::text[])
            AS l(indexer_id, guid, download_url, release_title)
        ON CONFLICT (indexer_id, guid) DO UPDATE SET
            download_url = EXCLUDED.download_url,
            release_title = EXCLUDED.release_title,
            updated_at = NOW()
        RETURNING indexer_id, guid, release_key, last_failure_at
        )
        SELECT indexer_id AS "indexer_id!", guid AS "guid!", release_key,
               COALESCE(last_failure_at > NOW() - INTERVAL '24 hours', false) AS "failed_recently!"
        FROM upserted
        "#,
        &indexer_ids,
        &guids,
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
                    guid: r.guid,
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
        r#"SELECT download_url, release_title, release_key FROM usenet_releases
           WHERE indexer_id = $1 AND guid = $2"#,
        nzb_ref.indexer_id,
        nzb_ref.guid,
    )
    .fetch_optional(exec)
    .await?;
    Ok(row)
}

pub async fn set_usenet_release_key<'e, E>(
    exec: E,
    indexer_id: &str,
    guid: &str,
    release_key: &str,
) -> Result<bool>
where
    E: Executor<'e, Database = Postgres>,
{
    let result = sqlx::query!(
        r#"UPDATE usenet_releases
           SET release_key = $3, last_failure_at = NULL, last_failure = NULL, updated_at = NOW()
           WHERE indexer_id = $1 AND guid = $2"#,
        indexer_id,
        guid,
        release_key,
    )
    .execute(exec)
    .await?;
    Ok(result.rows_affected() > 0)
}

pub async fn aliases<'e, E>(exec: E, release_key: &str) -> Result<Vec<NzbRef>>
where
    E: Executor<'e, Database = Postgres>,
{
    let refs = sqlx::query_as!(
        NzbRef,
        r#"SELECT indexer_id, guid FROM usenet_releases WHERE release_key = $1
           ORDER BY indexer_id, guid"#,
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
        r#"UPDATE usenet_releases SET last_failure_at = NOW(), last_failure = $3, updated_at = NOW()
           WHERE indexer_id = $1 AND guid = $2"#,
        nzb_ref.indexer_id,
        nzb_ref.guid,
        reason,
    )
    .execute(exec)
    .await?;
    Ok(())
}
