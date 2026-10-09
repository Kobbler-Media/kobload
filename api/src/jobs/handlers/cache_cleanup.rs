//! Handler for cleaning up expired search cache entries.
//!
//! Runs periodically to prune expired entries from the search_cache table.

use crate::jobs::JobError;
use crate::jobs::worker::JobHandler;
use crate::repositories::{search_cache, xem_cache};
use async_trait::async_trait;
use serde_json::Value;
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};
use uuid::Uuid;

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

        Ok(serde_json::json!({
            "search_cache_deleted": deleted,
            "xem_cache_deleted": xem_deleted
        }))
    }
}
