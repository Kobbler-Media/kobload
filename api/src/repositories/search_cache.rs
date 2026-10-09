use sha2::{Digest, Sha256};
use sqlx::{Executor, Postgres};
use uuid::Uuid;

use super::Result;

/// Build a deterministic cache key from search parameters.
///
/// `media_want` has to be part of the key: structured searches send an empty
/// query and carry the whole request in `media_want`, so keying on the query
/// alone lumps every episode/movie/book into one entry.
pub fn build_cache_key(
    source_id: Option<Uuid>,
    query: &str,
    categories: &[i32],
    limit: Option<u32>,
    offset: Option<u32>,
    media_want: Option<&nats_jobs::MediaWant>,
) -> String {
    let mut cats = categories.to_vec();
    cats.sort();
    let raw = format!(
        "{}:{}:{}:{}:{}:{}",
        source_id.map(|id| id.to_string()).unwrap_or_default(),
        query.trim().to_lowercase(),
        cats.iter()
            .map(|c| c.to_string())
            .collect::<Vec<_>>()
            .join(","),
        limit.unwrap_or(0),
        offset.unwrap_or(0),
        media_want
            .and_then(|mw| serde_json::to_string(mw).ok())
            .unwrap_or_default(),
    );
    const_hex::encode(Sha256::digest(raw.as_bytes()))
}

/// Look up cached search results. Returns None if not found or expired.
pub async fn get<'e, E>(exec: E, cache_key: &str) -> Result<Option<serde_json::Value>>
where
    E: Executor<'e, Database = Postgres>,
{
    let row = sqlx::query_scalar!(
        "SELECT results_json FROM search_cache WHERE cache_key = $1 AND expires_at > NOW()",
        cache_key,
    )
    .fetch_optional(exec)
    .await?;

    Ok(row)
}

/// Store search results in the cache.
pub async fn put<'e, E>(
    exec: E,
    cache_key: &str,
    results_json: &serde_json::Value,
    ttl_seconds: i64,
) -> Result<()>
where
    E: Executor<'e, Database = Postgres>,
{
    sqlx::query!(
        r#"
        INSERT INTO search_cache (id, cache_key, results_json, created_at, expires_at)
        VALUES ($1, $2, $3, NOW(), NOW() + make_interval(secs => $4))
        ON CONFLICT (cache_key) DO UPDATE SET
            results_json = EXCLUDED.results_json,
            created_at = EXCLUDED.created_at,
            expires_at = EXCLUDED.expires_at
        "#,
        Uuid::now_v7(),
        cache_key,
        results_json,
        ttl_seconds as f64,
    )
    .execute(exec)
    .await?;

    Ok(())
}

/// Delete expired cache entries.
pub async fn prune_expired<'e, E>(exec: E) -> Result<u64>
where
    E: Executor<'e, Database = Postgres>,
{
    let result = sqlx::query!("DELETE FROM search_cache WHERE expires_at < NOW()")
        .execute(exec)
        .await?;

    Ok(result.rows_affected())
}
