//! Hourly refresh of the review queue: missed wants and doubtful grabs.

use async_trait::async_trait;
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use tracing::info;
use uuid::Uuid;

use crate::jobs::JobError;
use crate::jobs::worker::JobHandler;
use crate::state::AppState;

pub struct ReviewQueueHandler {
    state: AppState,
}

impl ReviewQueueHandler {
    pub fn new(state: AppState) -> Self {
        Self { state }
    }
}

#[async_trait]
impl JobHandler for ReviewQueueHandler {
    fn job_type(&self) -> &'static str {
        "review_queue"
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
        let stats = crate::review::refresh(&self.state, crate::clock::now_utc())
            .await
            .map_err(|e| JobError::ExecutionFailed(e.to_string()))?;
        if stats.missed + stats.doubtful > 0 || stats.resolved > 0 {
            info!(
                missed = stats.missed,
                doubtful = stats.doubtful,
                resolved = stats.resolved,
                "Review queue refreshed"
            );
        }
        serde_json::to_value(&stats).map_err(|e| JobError::ExecutionFailed(e.to_string()))
    }
}
