//! Handler for updating Cardigann definitions from GitHub.

use async_trait::async_trait;
use serde_json::Value;
use std::path::Path;
use tokio_util::sync::CancellationToken;
use tracing::info;
use uuid::Uuid;

use crate::indexers::cardigann::{DefinitionCache, reload_definitions};
use crate::jobs::JobError;
use crate::jobs::worker::JobHandler;
use crate::routes::definitions::download_definitions;

pub struct DefinitionUpdateHandler {
    http_client: reqwest::Client,
    definitions: DefinitionCache,
}

impl DefinitionUpdateHandler {
    pub fn new(http_client: reqwest::Client, definitions: DefinitionCache) -> Self {
        Self {
            http_client,
            definitions,
        }
    }
}

#[async_trait]
impl JobHandler for DefinitionUpdateHandler {
    fn job_type(&self) -> &'static str {
        "definition_update"
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

        info!("Starting definition update from GitHub");

        let definitions_dir = std::env::var("CARDIGANN_DEFINITIONS_DIR")
            .unwrap_or_else(|_| "/data/definitions".to_string());
        let override_dir = std::env::var("CARDIGANN_DEFINITIONS_OVERRIDE_DIR").ok();

        let http = self.http_client.clone();
        let dir = definitions_dir.clone();
        let count = tokio::task::spawn_blocking(move || download_definitions(&dir, &http))
            .await
            .map_err(|e| JobError::ExecutionFailed(format!("task panicked: {e}")))?
            .map_err(JobError::ExecutionFailed)?;

        reload_definitions(
            &self.definitions,
            Path::new(&definitions_dir),
            override_dir.as_deref().map(Path::new),
        );

        let total = self.definitions.len();
        info!(downloaded = count, total, "Definition update completed");

        Ok(serde_json::json!({
            "downloaded": count,
            "total": total,
        }))
    }
}
