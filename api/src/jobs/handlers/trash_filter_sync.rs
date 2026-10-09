//! Handler for syncing TRaSH Guides filter data from GitHub.
//!
//! Runs daily to refresh the community-curated release quality filters.

use std::sync::Arc;

use async_trait::async_trait;
use reqwest::Client;
use serde_json::Value;
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;
use tracing::info;
use uuid::Uuid;

use crate::jobs::JobError;
use crate::jobs::worker::JobHandler;
use crate::trash_filters::TrashFilterCache;

pub struct TrashFilterSyncHandler {
    pool: PgPool,
    http_client: Client,
    trash_cache: Arc<TrashFilterCache>,
}

impl TrashFilterSyncHandler {
    pub fn new(pool: PgPool, http_client: Client, trash_cache: Arc<TrashFilterCache>) -> Self {
        Self {
            pool,
            http_client,
            trash_cache,
        }
    }
}

#[async_trait]
impl JobHandler for TrashFilterSyncHandler {
    fn job_type(&self) -> &'static str {
        "trash_filter_sync"
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

        info!("Starting TRaSH Guides filter sync");

        self.trash_cache
            .refresh(&self.http_client, &self.pool)
            .await;

        let count = self
            .trash_cache
            .get_filters()
            .map(|f| f.stats())
            .unwrap_or((0, 0));

        info!(
            blocklist_rules = count.0,
            tier_bonuses = count.1,
            "TRaSH Guides filter sync complete"
        );

        Ok(serde_json::json!({
            "blocklist_rules": count.0,
            "tier_bonuses": count.1,
        }))
    }
}
