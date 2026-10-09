//! Watch directory notify handler.
//!
//! Dispatches callbacks for discovered files to the configured callback URL.
//! Builds a payload compatible with Kobbler's POST /api/v1/admin/ingest/kobload.

use crate::jobs::JobError;
use crate::jobs::worker::JobHandler;
use crate::repositories::watch_directories as wd_repo;
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;
use tracing::{info, trace, warn};
use uuid::Uuid;

use crate::state::AppState;

#[derive(Debug, Deserialize, Serialize)]
pub struct WatchDirectoryNotifyPayload {
    pub watch_directory_id: Uuid,
}

pub struct WatchDirectoryNotifyHandler {
    pool: PgPool,
    http_client: Client,
    public_base_url: String,
}

impl WatchDirectoryNotifyHandler {
    pub fn new(state: &AppState) -> Self {
        Self {
            pool: state.pool.clone(),
            http_client: state.http_client.clone(),
            public_base_url: state.public_base_url.clone(),
        }
    }
}

#[async_trait]
impl JobHandler for WatchDirectoryNotifyHandler {
    fn job_type(&self) -> &'static str {
        "watch_directory_notify"
    }

    fn max_concurrent(&self) -> usize {
        2
    }

    async fn execute(
        &self,
        _job_id: Uuid,
        payload: Value,
        cancel: CancellationToken,
    ) -> Result<Value, JobError> {
        nats_jobs::check_cancelled(&cancel)?;

        let payload: WatchDirectoryNotifyPayload = serde_json::from_value(payload)
            .map_err(|e| JobError::ExecutionFailed(format!("Invalid payload: {}", e)))?;

        let dir = wd_repo::get_by_id(&self.pool, payload.watch_directory_id)
            .await
            .map_err(|e| JobError::ExecutionFailed(e.to_string()))?
            .ok_or_else(|| JobError::ExecutionFailed("Watch directory not found".to_string()))?;

        let files = wd_repo::get_undispatched_files(&self.pool, dir.id)
            .await
            .map_err(|e| JobError::ExecutionFailed(e.to_string()))?;

        if files.is_empty() {
            return Ok(json!({ "sent": 0, "errors": 0 }));
        }

        let mut sent = 0;
        let mut errors = 0;

        for file in &files {
            match self.send_callback(&dir, file).await {
                Ok(()) => {
                    let _ = wd_repo::mark_file_sent(&self.pool, file.id).await;
                    sent += 1;
                    trace!(file_id = %file.id, file_name = %file.file_name, "Callback sent");
                }
                Err(e) => {
                    let error_msg = e.to_string();
                    warn!(file_id = %file.id, error = %error_msg, "Callback failed");
                    let _ = wd_repo::mark_file_error(&self.pool, file.id, &error_msg).await;
                    errors += 1;
                }
            }
        }

        info!(
            directory = %dir.name,
            sent = sent,
            errors = errors,
            total = files.len(),
            "Notify dispatch complete"
        );

        Ok(json!({ "sent": sent, "errors": errors }))
    }
}

impl WatchDirectoryNotifyHandler {
    async fn send_callback(
        &self,
        dir: &wd_repo::WatchDirectory,
        file: &wd_repo::WatchDirectoryFile,
    ) -> Result<(), CallbackError> {
        let file_url = format!(
            "{}/api/v1/watch-directories/{}/files/{}/content",
            self.public_base_url, dir.id, file.id
        );

        let callback_payload = json!({
            "source_type": "watch_directory",
            "watch_directory_id": dir.id,
            "file_id": file.id,
            "file_name": file.file_name,
            "file_path": file.file_path,
            "file_size": file.file_size,
            "file_hash": file.file_hash,
            "file_url": file_url,
            "media_type": dir.media_type,
            "release_interpretation": file.interpretation,
            "metadata": {
                "source_type": "watch_directory",
                "watch_directory_id": dir.id,
                "watch_directory_name": dir.name,
            }
        });

        let mut req = self
            .http_client
            .post(&dir.callback_url)
            .json(&callback_payload);

        if let Some(ref token) = dir.auth_token {
            req = req.header("Authorization", format!("Bearer {}", token));
        }

        let resp = req
            .send()
            .await
            .map_err(|e| CallbackError(format!("HTTP request failed: {}", e)))?;

        let status = resp.status();
        if status.is_success() {
            Ok(())
        } else {
            let body = resp.text().await.unwrap_or_default();
            Err(CallbackError(format!(
                "HTTP {} from callback: {}",
                status, body
            )))
        }
    }
}

#[derive(Debug)]
struct CallbackError(String);

impl std::fmt::Display for CallbackError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}
