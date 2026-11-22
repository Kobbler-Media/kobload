use std::sync::Arc;

use async_trait::async_trait;
use axum::http::StatusCode;
use base64::Engine as _;
use usenet_proto::{JobFailure, JobRequest, JobState, JobStatus};

use super::registry::{DownloadClientCapabilities, DownloadClientHandler, NewDownload};
use super::storage::scan_storage_path;
use super::{DownloadClientError, PollResult};
use crate::types::{DownloadClientConfig, TestConnectionResult};
use crate::usenet::{EngineError, UsenetEngine};

pub struct UsenetHandler {
    engine: Option<Arc<UsenetEngine>>,
}

impl UsenetHandler {
    pub fn new(engine: Option<Arc<UsenetEngine>>) -> Self {
        Self { engine }
    }

    fn engine(&self) -> Result<&UsenetEngine, DownloadClientError> {
        self.engine.as_deref().ok_or_else(|| {
            DownloadClientError::Message(
                "the usenet engine is not configured (USENET_ENGINE_URL)".to_string(),
            )
        })
    }
}

impl From<EngineError> for DownloadClientError {
    fn from(err: EngineError) -> Self {
        match err {
            EngineError::Request(e) => DownloadClientError::Request(e),
            EngineError::Unavailable(_) => {
                DownloadClientError::Status(StatusCode::SERVICE_UNAVAILABLE)
            }
            EngineError::Status(code, _) if code >= 500 => DownloadClientError::Status(
                StatusCode::from_u16(code).unwrap_or(StatusCode::BAD_GATEWAY),
            ),
            e => DownloadClientError::Message(e.to_string()),
        }
    }
}

fn client_data(job: &JobStatus) -> serde_json::Value {
    serde_json::json!({
        "eta_time": None::<String>,
        "download_rate": None::<i64>,
        "total_size": job.total_bytes,
        "job_id": job.id,
        "state": job.state,
        "progress": job.progress,
        "downloaded_bytes": job.downloaded_bytes,
        "storage": job.storage,
        "error": job.error,
        "failure": job.failure,
    })
}

async fn poll_result(job: &JobStatus) -> PollResult {
    let data = client_data(job);
    let progress = Some(job.progress.clamp(0.0, 1.0));
    match (job.state, job.failure) {
        (JobState::Completed, _) => {
            let scan = scan_storage_path(job.storage.as_deref().unwrap_or_default()).await;
            match scan.error {
                Some(error) => PollResult::completed_scan_error(error, data),
                None => PollResult::completed_with_data(scan.files, data),
            }
        }
        // `usenet::reconcile_jobs` runs before every poll and owns infrastructure failures:
        // it releases the job and requeues the download, so the release is never excluded.
        (JobState::Failed, Some(JobFailure::Infrastructure)) => {
            let mut result = PollResult::downloading(progress);
            result.client_data = Some(data);
            result
        }
        (JobState::Failed, _) => {
            let error = job
                .error
                .clone()
                .unwrap_or_else(|| "usenet engine job failed".to_string());
            let mut result = PollResult::failed(error, progress);
            result.client_data = Some(data);
            result
        }
        _ => {
            let mut result = PollResult::downloading(progress);
            result.client_data = Some(data);
            result
        }
    }
}

#[async_trait]
impl DownloadClientHandler for UsenetHandler {
    fn client_type(&self) -> &'static str {
        "usenet"
    }

    fn capabilities(&self) -> DownloadClientCapabilities {
        DownloadClientCapabilities {
            supports_add_url: false,
            supports_add_bytes: true,
        }
    }

    async fn add_url(
        &self,
        _http_client: &reqwest::Client,
        _client: &DownloadClientConfig,
        _url: &str,
    ) -> Result<String, DownloadClientError> {
        Err(DownloadClientError::Message(
            "the usenet engine needs an NZB file, not a URL".to_string(),
        ))
    }

    async fn add_bytes(
        &self,
        _http_client: &reqwest::Client,
        client: &DownloadClientConfig,
        bytes: &[u8],
        download: &NewDownload<'_>,
    ) -> Result<String, DownloadClientError> {
        let category = client
            .category
            .as_deref()
            .map(str::trim)
            .filter(|c| !c.is_empty())
            .ok_or_else(|| {
                DownloadClientError::Message("the usenet client has no category".to_string())
            })?;
        let job = JobRequest {
            id: download.id.to_string(),
            name: download.title.to_string(),
            category: category.to_string(),
            nzb_base64: base64::engine::general_purpose::STANDARD.encode(bytes),
            password: download.password.map(str::to_string),
        };
        Ok(self.engine()?.add_job(&job).await?.id)
    }

    async fn poll(
        &self,
        _http_client: &reqwest::Client,
        _client: &DownloadClientConfig,
        external_id: &str,
    ) -> Result<PollResult, DownloadClientError> {
        let job = self.engine()?.job(external_id).await?;
        Ok(poll_result(&job).await)
    }

    async fn remove(
        &self,
        _http_client: &reqwest::Client,
        _client: &DownloadClientConfig,
        external_id: &str,
        delete_data: bool,
    ) -> Result<(), DownloadClientError> {
        Ok(self.engine()?.delete_job(external_id, delete_data).await?)
    }

    async fn test_connection(
        &self,
        _http_client: &reqwest::Client,
        _client: &DownloadClientConfig,
    ) -> TestConnectionResult {
        let result = match self.engine() {
            Ok(engine) => engine.health().await.map_err(|e| e.to_string()),
            Err(e) => Err(e.to_string()),
        };
        match result {
            Ok(()) => TestConnectionResult {
                success: true,
                message: "Connected to the usenet engine".to_string(),
            },
            Err(message) => TestConnectionResult {
                success: false,
                message,
            },
        }
    }
}
