use std::time::Duration;

use serde::de::DeserializeOwned;
use usenet_proto::{
    HealthSample, ImportRequest, ImportResponse, JobRequest, JobStatus, Purpose, StreamClaims,
};

/// kobbler keeps a resolved URL this long before it resolves again.
pub const STREAM_TTL: Duration = Duration::from_secs(6 * 3600);

/// A pack import reads the first segment of every file before it answers.
const IMPORT_TIMEOUT: Duration = Duration::from_secs(90);

/// A 5% STAT sample of a long file runs to hundreds of STATs on the health lane, which
/// yields to every other workload.
const HEALTH_TIMEOUT: Duration = Duration::from_secs(300);

const REFRESH_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Clone)]
pub struct EngineSettings {
    pub engine_url: String,
    pub stream_base_url: String,
    pub control_key: String,
    pub stream_secret: String,
}

impl std::fmt::Debug for EngineSettings {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EngineSettings")
            .field("engine_url", &self.engine_url)
            .field("stream_base_url", &self.stream_base_url)
            .finish_non_exhaustive()
    }
}

impl EngineSettings {
    pub fn from_env() -> Result<Option<Self>, String> {
        Self::from_lookup(|name| std::env::var(name).ok())
    }

    /// Unset means usenet streaming is off; set partly, or with weak or shared secrets,
    /// kobload refuses to start rather than run with a guessable key.
    pub fn from_lookup(lookup: impl Fn(&str) -> Option<String>) -> Result<Option<Self>, String> {
        const NAMES: [&str; 4] = [
            "USENET_ENGINE_URL",
            "USENET_STREAM_BASE_URL",
            "USENET_CONTROL_KEY",
            "USENET_STREAM_SECRET",
        ];
        let [engine_url, stream_base_url, control_key, stream_secret] = NAMES.map(|name| {
            lookup(name)
                .map(|v| v.trim().trim_end_matches('/').to_string())
                .filter(|v| !v.is_empty())
        });
        let (Some(engine_url), Some(stream_base_url), Some(control_key), Some(stream_secret)) =
            (engine_url, stream_base_url, control_key, stream_secret)
        else {
            let set: Vec<&str> = NAMES
                .into_iter()
                .filter(|name| lookup(name).is_some_and(|v| !v.trim().is_empty()))
                .collect();
            return match set.as_slice() {
                [] => Ok(None),
                set => Err(format!(
                    "The usenet engine needs all of {} set, but only {} are",
                    NAMES.join(", "),
                    set.join(", ")
                )),
            };
        };
        usenet_proto::check_secrets(&[
            ("USENET_CONTROL_KEY", &control_key),
            ("USENET_STREAM_SECRET", &stream_secret),
        ])
        .map_err(|e| e.to_string())?;
        Ok(Some(Self {
            engine_url,
            stream_base_url,
            control_key,
            stream_secret,
        }))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("the usenet engine has no copy of this release")]
    NotCached,
    #[error("the usenet engine cannot use this release: {0}")]
    Rejected(String),
    #[error("no usenet provider is available right now: {0}")]
    Unavailable(String),
    #[error("the usenet engine answered {0}: {1}")]
    Status(u16, String),
    #[error("the usenet engine has no job {0}")]
    UnknownJob(String),
    #[error("the usenet engine is unreachable: {0}")]
    Request(#[from] reqwest::Error),
}

pub struct UsenetEngine {
    settings: EngineSettings,
    http: reqwest::Client,
}

impl UsenetEngine {
    pub fn new(settings: EngineSettings, http: reqwest::Client) -> Self {
        Self { settings, http }
    }

    pub fn accepts_control_key(&self, token: &str) -> bool {
        usenet_proto::keys_match(token, &self.settings.control_key)
    }

    pub async fn import(&self, request: &ImportRequest) -> Result<ImportResponse, EngineError> {
        let resp = self
            .http
            .post(format!("{}/v1/import", self.settings.engine_url))
            .bearer_auth(&self.settings.control_key)
            .timeout(IMPORT_TIMEOUT)
            .json(request)
            .send()
            .await?;
        decode(resp).await
    }

    pub async fn release(&self, release_key: &str) -> Result<ImportResponse, EngineError> {
        let resp = self
            .http
            .get(format!(
                "{}/v1/releases/{release_key}",
                self.settings.engine_url
            ))
            .bearer_auth(&self.settings.control_key)
            .send()
            .await?;
        decode(resp).await
    }

    pub async fn sample_health(
        &self,
        release_key: &str,
        percent: u8,
        output: usize,
    ) -> Result<HealthSample, EngineError> {
        self.release_health(
            release_key,
            ("percent", usize::from(percent)),
            output,
            HEALTH_TIMEOUT,
        )
        .await
    }

    pub async fn quick_health(
        &self,
        release_key: &str,
        articles: usize,
        output: usize,
        budget: Duration,
    ) -> Result<HealthSample, EngineError> {
        self.release_health(release_key, ("articles", articles), output, budget)
            .await
    }

    async fn release_health(
        &self,
        release_key: &str,
        amount: (&str, usize),
        output: usize,
        timeout: Duration,
    ) -> Result<HealthSample, EngineError> {
        let resp = self
            .http
            .post(format!(
                "{}/v1/releases/{release_key}/health",
                self.settings.engine_url
            ))
            .query(&[amount])
            .query(&[("output", output)])
            .bearer_auth(&self.settings.control_key)
            .timeout(timeout)
            .send()
            .await?;
        decode(resp).await
    }

    pub async fn add_job(&self, job: &JobRequest) -> Result<JobStatus, EngineError> {
        let resp = self
            .http
            .post(self.jobs_url())
            .bearer_auth(&self.settings.control_key)
            .json(job)
            .send()
            .await?;
        decode(resp).await
    }

    pub async fn jobs(&self) -> Result<Vec<JobStatus>, EngineError> {
        let resp = self
            .http
            .get(self.jobs_url())
            .bearer_auth(&self.settings.control_key)
            .send()
            .await?;
        decode(resp).await
    }

    pub async fn job(&self, id: &str) -> Result<JobStatus, EngineError> {
        let resp = self
            .http
            .get(format!("{}/{id}", self.jobs_url()))
            .bearer_auth(&self.settings.control_key)
            .send()
            .await?;
        decode(resp).await.map_err(|e| match e {
            EngineError::NotCached => EngineError::UnknownJob(id.to_string()),
            e => e,
        })
    }

    pub async fn delete_job(&self, id: &str, delete_files: bool) -> Result<(), EngineError> {
        let resp = self
            .http
            .delete(format!("{}/{id}", self.jobs_url()))
            .query(&[("delete_files", delete_files)])
            .bearer_auth(&self.settings.control_key)
            .send()
            .await?;
        if resp.status().is_success() || resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(());
        }
        Err(error_of(resp).await)
    }

    fn jobs_url(&self) -> String {
        format!("{}/v1/jobs", self.settings.engine_url)
    }

    // Best effort: the engine pulls on its own within a minute anyway.
    pub async fn refresh_config(&self) {
        let sent = self
            .http
            .post(format!("{}/v1/config/refresh", self.settings.engine_url))
            .bearer_auth(&self.settings.control_key)
            .timeout(REFRESH_TIMEOUT)
            .send()
            .await
            .and_then(reqwest::Response::error_for_status);
        if let Err(e) = sent {
            tracing::warn!(error = %e.without_url(), "Could not ask the usenet engine to pull its config");
        }
    }

    pub async fn health(&self) -> Result<(), EngineError> {
        let resp = self
            .http
            .get(format!("{}/healthz", self.settings.engine_url))
            .send()
            .await?;
        if resp.status().is_success() {
            Ok(())
        } else {
            Err(EngineError::Status(resp.status().as_u16(), String::new()))
        }
    }

    pub fn stream_url(
        &self,
        release_key: &str,
        output: usize,
        purpose: Purpose,
        expires_at: i64,
    ) -> String {
        let claims = StreamClaims {
            release_key: release_key.to_string(),
            output,
            purpose,
            expires_at,
        };
        let token = usenet_proto::sign(self.settings.stream_secret.as_bytes(), &claims);
        format!("{}/stream/{token}", self.settings.stream_base_url)
    }
}

async fn decode<T: DeserializeOwned>(resp: reqwest::Response) -> Result<T, EngineError> {
    if resp.status().is_success() {
        return Ok(resp.json().await?);
    }
    Err(error_of(resp).await)
}

async fn error_of(resp: reqwest::Response) -> EngineError {
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    let reason = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v["error"].as_str().map(str::to_string))
        .unwrap_or_else(|| body.chars().take(200).collect());
    match status.as_u16() {
        404 => EngineError::NotCached,
        422 => EngineError::Rejected(reason),
        503 => EngineError::Unavailable(reason),
        code => EngineError::Status(code, reason),
    }
}
