//! Handler for syncing indexer sources from Prowlarr.
//!
//! This job runs periodically to keep Kobload's indexer sources in sync
//! with the enabled indexers in Prowlarr.

use crate::jobs::JobError;
use crate::jobs::worker::JobHandler;
use crate::repositories::watch as watch_repo;
use crate::watch::prowlarr_sync::sync_prowlarr_indexers;
use async_trait::async_trait;
use serde_json::Value;
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};
use uuid::Uuid;

/// Handler for indexer source sync jobs.
pub struct IndexerSyncHandler {
    pool: PgPool,
    http_client: reqwest::Client,
}

impl IndexerSyncHandler {
    pub fn new(pool: PgPool, http_client: reqwest::Client) -> Self {
        Self { pool, http_client }
    }
}

#[async_trait]
impl JobHandler for IndexerSyncHandler {
    fn job_type(&self) -> &'static str {
        "indexer_sync"
    }

    /// Only 1 at a time - no need for concurrent syncs.
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

        let prowlarr_sources = watch_repo::list_indexer_sources_by_type(&self.pool, "prowlarr")
            .await
            .map_err(|e| JobError::ExecutionFailed(e.to_string()))?;

        if prowlarr_sources.is_empty() {
            debug!("Indexer sync skipped - no Prowlarr sources configured");
            return Ok(serde_json::json!({
                "skipped": true,
                "reason": "no_prowlarr_sources"
            }));
        }

        let mut total_created = 0;
        let mut total_updated = 0;
        let mut total_disabled = 0;
        let mut all_indexers = Vec::new();

        for source in &prowlarr_sources {
            if !source.enabled {
                continue;
            }
            match sync_prowlarr_indexers(&self.pool, &self.http_client, source.id).await {
                Ok(result) => {
                    total_created += result.created;
                    total_updated += result.updated;
                    total_disabled += result.disabled;
                    all_indexers.extend(result.indexers);
                }
                Err(e) => {
                    tracing::warn!(
                        source_id = %source.id,
                        source_name = %source.name,
                        error = %e,
                        "Failed to sync Prowlarr instance"
                    );
                }
            }
        }

        if total_created > 0 || total_updated > 0 || total_disabled > 0 {
            info!(
                created = total_created,
                updated = total_updated,
                disabled = total_disabled,
                "Indexer sync completed"
            );
        } else {
            info!("Indexer sync completed - no changes");
        }

        Ok(serde_json::json!({
            "created": total_created,
            "updated": total_updated,
            "disabled": total_disabled,
            "indexers": all_indexers
        }))
    }
}
