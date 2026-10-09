//! Handler for cleaning up watch patterns whose upgrade window has expired.
//!
//! Runs periodically to delete patterns where `upgrade_until` is in the past.

use crate::jobs::JobError;
use crate::jobs::worker::JobHandler;
use crate::repositories::watch;
use async_trait::async_trait;
use serde_json::Value;
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};
use uuid::Uuid;

/// Handler for upgrade cleanup jobs.
pub struct UpgradeCleanupHandler {
    pool: PgPool,
}

impl UpgradeCleanupHandler {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl JobHandler for UpgradeCleanupHandler {
    fn job_type(&self) -> &'static str {
        "upgrade_cleanup"
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

        let deleted = watch::delete_expired_upgrades(&self.pool)
            .await
            .map_err(|e| JobError::ExecutionFailed(e.to_string()))?;

        if deleted > 0 {
            info!(
                deleted = deleted,
                "Deleted watch patterns with expired upgrade windows"
            );
        } else {
            debug!("No expired upgrade windows to clean up");
        }

        Ok(serde_json::json!({
            "deleted": deleted
        }))
    }
}
