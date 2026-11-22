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

#[derive(Debug, Clone)]
pub struct UsenetRelease {
    pub download_url: String,
    pub release_title: Option<String>,
    pub release_key: Option<String>,
}

pub async fn upsert<'e, E>(exec: E, listings: &[Listing]) -> Result<HashMap<NzbRef, String>>
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
        INSERT INTO usenet_releases (indexer_id, guid, download_url, release_title)
        SELECT DISTINCT ON (indexer_id, guid) indexer_id, guid, download_url, release_title
        FROM UNNEST($1::text[], $2::text[], $3::text[], $4::text[])
            AS l(indexer_id, guid, download_url, release_title)
        ON CONFLICT (indexer_id, guid) DO UPDATE SET
            download_url = EXCLUDED.download_url,
            release_title = EXCLUDED.release_title,
            updated_at = NOW()
        RETURNING indexer_id, guid, release_key
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
        .filter_map(|r| {
            let key = r.release_key?;
            Some((
                NzbRef {
                    indexer_id: r.indexer_id,
                    guid: r.guid,
                },
                key,
            ))
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
        r#"UPDATE usenet_releases SET release_key = $3, updated_at = NOW()
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
