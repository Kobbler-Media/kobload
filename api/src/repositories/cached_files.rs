use sqlx::PgPool;
use uuid::Uuid;

const KEPT: usize = 8;

/// File-id sets that streamed from this torrent without downloading, newest first. RD's
/// cache is per selection: the same hash can be instant with one set and download with
/// another.
pub async fn sets(pool: &PgPool, provider_id: Uuid, info_hash: &str) -> Vec<Vec<i32>> {
    sqlx::query_scalar!(
        "SELECT file_data FROM debrid_availability_cache WHERE provider_id = $1 AND info_hash = $2",
        provider_id,
        info_hash.to_lowercase(),
    )
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
    .flatten()
    .and_then(|v| serde_json::from_value(v).ok())
    .unwrap_or_default()
}

pub async fn remember(pool: &PgPool, provider_id: Uuid, info_hash: &str, ids: &[i32]) {
    let mut set = ids.to_vec();
    set.sort_unstable();
    if set.is_empty() {
        return;
    }
    let mut all = sets(pool, provider_id, info_hash).await;
    all.retain(|s| *s != set);
    all.insert(0, set);
    all.truncate(KEPT);
    if let Err(e) = sqlx::query!(
        r#"INSERT INTO debrid_availability_cache (info_hash, provider_id, available, file_data)
           VALUES ($1, $2, true, $3)
           ON CONFLICT (info_hash, provider_id) DO UPDATE SET file_data = $3"#,
        info_hash.to_lowercase(),
        provider_id,
        serde_json::json!(all),
    )
    .execute(pool)
    .await
    {
        tracing::warn!(error = %e, info_hash, "Failed to remember cached file set");
    }
}
