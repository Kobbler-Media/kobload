//! Handler for refreshing Newznab indexer capabilities (caps).
//!
//! Runs daily. Fetches `/api?t=caps` for each Newznab source and stores
//! the parsed capabilities (supported search types and params) in the database.

use crate::indexers::newznab::fetch_caps;
use crate::jobs::JobError;
use crate::jobs::worker::JobHandler;
use crate::repositories::watch as watch_repo;
use async_trait::async_trait;
use serde_json::Value;
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use uuid::Uuid;

pub struct RefreshIndexerCapsHandler {
    pool: PgPool,
    http_client: reqwest::Client,
}

impl RefreshIndexerCapsHandler {
    pub fn new(pool: PgPool, http_client: reqwest::Client) -> Self {
        Self { pool, http_client }
    }
}

#[async_trait]
impl JobHandler for RefreshIndexerCapsHandler {
    fn job_type(&self) -> &'static str {
        "refresh_indexer_caps"
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

        let sources = watch_repo::list_indexer_sources(&self.pool)
            .await
            .map_err(|e| JobError::ExecutionFailed(e.to_string()))?;

        let newznab_sources: Vec<_> = sources
            .iter()
            .filter(|s| (s.handler_type == "newznab" || s.handler_type == "torznab") && s.enabled)
            .collect();

        let total = newznab_sources.len();
        let mut updated = 0;
        let mut errors = 0;

        for source in &newznab_sources {
            // Skip if caps were fetched recently (within 20 hours — gives buffer for daily schedule)
            if let Some(fetched_at) = source.caps_fetched_at {
                let age = time::OffsetDateTime::now_utc() - fetched_at;
                if age < time::Duration::hours(20) {
                    continue;
                }
            }

            match fetch_caps(&self.http_client, source).await {
                Ok(caps) => {
                    let caps_json = serde_json::to_value(&caps)
                        .map_err(|e| JobError::ExecutionFailed(e.to_string()))?;

                    watch_repo::update_indexer_caps(&self.pool, source.id, &caps_json)
                        .await
                        .map_err(|e| JobError::ExecutionFailed(e.to_string()))?;

                    info!(
                        source = %source.name,
                        tv_search = caps.tv_search.available,
                        movie_search = caps.movie_search.available,
                        "Updated indexer capabilities"
                    );
                    updated += 1;
                }
                Err(e) => {
                    warn!(source = %source.name, error = %e, "Failed to fetch indexer caps");
                    errors += 1;
                }
            }
        }

        info!(
            total,
            updated,
            errors,
            skipped = total - updated - errors,
            "Indexer caps refresh complete"
        );

        Ok(serde_json::json!({
            "total_newznab_sources": total,
            "updated": updated,
            "errors": errors,
        }))
    }
}
