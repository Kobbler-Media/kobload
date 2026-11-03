use sqlx::PgPool;
use uuid::Uuid;

use super::Result;

/// A stream added this torrent or came back for it.
pub async fn touched(
    pool: &PgPool,
    provider_id: Uuid,
    torrent_id: &str,
    info_hash: &str,
) -> Result<()> {
    sqlx::query!(
        "INSERT INTO debrid_stream_torrents (provider_id, torrent_id, info_hash)
         VALUES ($1, $2, $3)
         ON CONFLICT (provider_id, torrent_id) DO UPDATE SET touched_at = NOW()",
        provider_id,
        torrent_id,
        info_hash.to_lowercase(),
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Someone is still waiting on this torrent; a no-op for one no stream added.
pub async fn polled(pool: &PgPool, provider_id: Uuid, torrent_id: &str) -> Result<()> {
    sqlx::query!(
        "UPDATE debrid_stream_torrents SET touched_at = NOW()
         WHERE provider_id = $1 AND torrent_id = $2",
        provider_id,
        torrent_id,
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn removed(pool: &PgPool, provider_id: Uuid, torrent_id: &str) -> Result<()> {
    sqlx::query!(
        "DELETE FROM debrid_stream_torrents WHERE provider_id = $1 AND torrent_id = $2",
        provider_id,
        torrent_id,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// (torrent id, info hash) of torrents no stream has touched for this long, oldest first.
pub async fn untouched_for(
    pool: &PgPool,
    provider_id: Uuid,
    minutes: i32,
) -> Result<Vec<(String, String)>> {
    let rows = sqlx::query!(
        "SELECT torrent_id, info_hash FROM debrid_stream_torrents
         WHERE provider_id = $1 AND touched_at < NOW() - make_interval(mins => $2)
         ORDER BY touched_at
         LIMIT 20",
        provider_id,
        minutes,
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|r| (r.torrent_id, r.info_hash))
        .collect())
}

pub async fn prune(pool: &PgPool, days: i32) -> Result<u64> {
    let result = sqlx::query!(
        "DELETE FROM debrid_stream_torrents WHERE touched_at < NOW() - make_interval(days => $1)",
        days,
    )
    .execute(pool)
    .await?;
    Ok(result.rows_affected())
}
