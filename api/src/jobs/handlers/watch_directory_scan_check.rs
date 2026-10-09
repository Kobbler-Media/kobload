//! Watch directory scan check handler.
//!
//! Periodic check that enqueues `watch_directory_scan` jobs for all enabled
//! watch directories. Runs every 5 minutes via the NATS scheduler.

use crate::jobs::worker::JobHandler;
use crate::jobs::{self, JobContext, JobError};
use crate::repositories::watch_directories as wd_repo;
use async_trait::async_trait;
use serde_json::{Value, json};
use sqlx::PgPool;
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::nats::NatsState;

pub struct WatchDirectoryScanCheckHandler {
    pool: PgPool,
    nats: NatsState,
}

impl WatchDirectoryScanCheckHandler {
    pub fn new(pool: PgPool, nats: NatsState) -> Self {
        Self { pool, nats }
    }
}

#[async_trait]
impl JobHandler for WatchDirectoryScanCheckHandler {
    fn job_type(&self) -> &'static str {
        "watch_directory_scan_check"
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

        let directories = wd_repo::list_enabled(&self.pool)
            .await
            .map_err(|e| JobError::ExecutionFailed(e.to_string()))?;

        if directories.is_empty() {
            return Ok(json!({ "checked": 0, "enqueued": 0 }));
        }

        let mut checked = 0;
        let mut enqueued = 0;
        let mut skipped_backoff = 0;
        let mut skipped_active = 0;

        for dir in directories {
            checked += 1;

            // Check if in backoff period
            if let Some(retry_at) = dir.next_retry_at
                && retry_at > OffsetDateTime::now_utc()
            {
                debug!(
                    directory_id = %dir.id,
                    name = %dir.name,
                    "Skipping directory - in backoff"
                );
                skipped_backoff += 1;
                continue;
            }

            // Check if scan already active
            if dir.scan_status == "scanning" {
                debug!(
                    directory_id = %dir.id,
                    name = %dir.name,
                    "Skipping directory - scan already in progress"
                );
                skipped_active += 1;
                continue;
            }

            // Check for active job
            let dir_id_str = dir.id.to_string();
            match jobs::has_active_job(
                &self.pool,
                "watch_directory_scan",
                "watch_directory_id",
                &dir_id_str,
            )
            .await
            {
                Ok(true) => {
                    skipped_active += 1;
                    continue;
                }
                Ok(false) => {}
                Err(e) => {
                    warn!(directory_id = %dir.id, error = %e, "Failed to check active job");
                }
            }

            let context = JobContext::for_entity(
                "watch_directory",
                dir.id,
                format!("Scanning directory '{}'", dir.name),
            );

            if let Some(job_id) = jobs::enqueue_optional(
                &self.pool,
                &self.nats.publisher,
                "watch_directory_scan",
                json!({ "watch_directory_id": dir.id }),
                context,
            )
            .await
            {
                debug!(job_id = %job_id, directory_id = %dir.id, "Enqueued directory scan");
                enqueued += 1;
            }
        }

        if enqueued > 0 {
            info!(
                checked,
                enqueued, skipped_backoff, skipped_active, "Watch directory scan check complete"
            );
        }

        Ok(json!({
            "checked": checked,
            "enqueued": enqueued,
            "skipped_backoff": skipped_backoff,
            "skipped_active": skipped_active,
        }))
    }
}
