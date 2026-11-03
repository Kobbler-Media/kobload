//! Handler for cleaning up expired search cache entries.
//!
//! Runs periodically to prune expired search cache, XEM, stream outcome, stream
//! attempt and release sighting rows.

use crate::jobs::JobError;
use crate::jobs::worker::JobHandler;
use crate::repositories::{
    release_sightings, search_cache, stream_attempts, stream_torrents, xem_cache,
};
use async_trait::async_trait;
use serde_json::Value;
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};
use uuid::Uuid;

/// A stream's torrent still downloading after this long is forgotten rather than reclaimed.
const STREAM_TORRENT_DAYS: i32 = 7;

/// Handler for cache cleanup jobs.
pub struct CacheCleanupHandler {
    pool: PgPool,
}

impl CacheCleanupHandler {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl JobHandler for CacheCleanupHandler {
    fn job_type(&self) -> &'static str {
        "cache_cleanup"
    }

    fn max_concurrent(&self) -> usize {
        1
    }

    async fn execute(
        &self,
        _job_id: Uuid,
        _payload: Value,
        cancel: CancellationToken,
    ) -> Result<Value, JobError> {
        nats_jobs::check_cancelled(&cancel)?;

        let deleted = search_cache::prune_expired(&self.pool)
            .await
            .map_err(|e| JobError::ExecutionFailed(e.to_string()))?;

        if deleted > 0 {
            info!(deleted, "Pruned expired search cache entries");
        } else {
            debug!("No expired search cache entries to prune");
        }

        let xem_deleted = xem_cache::prune_stale(&self.pool, 168)
            .await
            .map_err(|e| JobError::ExecutionFailed(e.to_string()))?;

        if xem_deleted > 0 {
            info!(deleted = xem_deleted, "Pruned stale XEM cache entries");
        }

        let (releases_deleted, traits_deleted) =
            crate::services::stream::outcomes::prune(&self.pool)
                .await
                .map_err(|e| JobError::ExecutionFailed(e.to_string()))?;
        if releases_deleted + traits_deleted > 0 {
            info!(
                releases_deleted,
                traits_deleted, "Pruned expired stream outcomes"
            );
        }

        let sightings_deleted = release_sightings::prune(&self.pool)
            .await
            .map_err(|e| JobError::ExecutionFailed(e.to_string()))?;
        if sightings_deleted > 0 {
            info!(deleted = sightings_deleted, "Pruned old release sightings");
        }

        let attempts_abandoned = stream_attempts::abandon_stale(
            &self.pool,
            stream_attempts::STALE_MINUTES,
            &format!(
                "No activity for {} minutes; the client stopped waiting",
                stream_attempts::STALE_MINUTES
            ),
        )
        .await
        .map_err(|e| JobError::ExecutionFailed(e.to_string()))?;
        let attempts_deleted = stream_attempts::prune(&self.pool, stream_attempts::RETENTION_HOURS)
            .await
            .map_err(|e| JobError::ExecutionFailed(e.to_string()))?;
        if attempts_deleted > 0 {
            info!(deleted = attempts_deleted, "Pruned old stream attempts");
        }

        let stream_torrents_deleted = stream_torrents::prune(&self.pool, STREAM_TORRENT_DAYS)
            .await
            .map_err(|e| JobError::ExecutionFailed(e.to_string()))?;

        let probes_ingested = crate::services::stream::outcomes::ingest_past_probes(&self.pool)
            .await
            .map_err(|e| JobError::ExecutionFailed(e.to_string()))?;
        if probes_ingested > 0 {
            info!(
                probes_ingested,
                "Learned release traits from past availability probes"
            );
        }

        Ok(serde_json::json!({
            "search_cache_deleted": deleted,
            "xem_cache_deleted": xem_deleted,
            "stream_release_outcomes_deleted": releases_deleted,
            "stream_trait_outcomes_deleted": traits_deleted,
            "release_sightings_deleted": sightings_deleted,
            "stream_attempts_abandoned": attempts_abandoned,
            "stream_attempts_deleted": attempts_deleted,
            "stream_torrents_deleted": stream_torrents_deleted,
            "stream_probes_ingested": probes_ingested
        }))
    }
}
