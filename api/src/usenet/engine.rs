use std::time::Duration;

use subtle::ConstantTimeEq;
use usenet_proto::{ImportRequest, ImportResponse, Purpose, StreamClaims};

/// kobbler keeps a resolved URL this long before it resolves again.
pub const STREAM_TTL: Duration = Duration::from_secs(6 * 3600);

/// A pack import reads the first segment of every file before it answers.
const IMPORT_TIMEOUT: Duration = Duration::from_secs(90);

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
    pub fn from_env() -> Option<Self> {
        let var = |name: &str| {
            std::env::var(name)
                .ok()
                .map(|v| v.trim().trim_end_matches('/').to_string())
                .filter(|v| !v.is_empty())
        };
        Some(Self {
            engine_url: var("USENET_ENGINE_URL")?,
            stream_base_url: var("USENET_STREAM_BASE_URL")?,
            control_key: var("USENET_CONTROL_KEY")?,
            stream_secret: var("USENET_STREAM_SECRET")?,
        })
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
        let expected = self.settings.control_key.as_bytes();
        token.len() == expected.len() && bool::from(token.as_bytes().ct_eq(expected))
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

async fn decode(resp: reqwest::Response) -> Result<ImportResponse, EngineError> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp.json().await?);
    }
    let body = resp.text().await.unwrap_or_default();
    let reason = serde_json::from_str::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v["error"].as_str().map(str::to_string))
        .unwrap_or_else(|| body.chars().take(200).collect());
    Err(match status.as_u16() {
        404 => EngineError::NotCached,
        422 => EngineError::Rejected(reason),
        503 => EngineError::Unavailable(reason),
        code => EngineError::Status(code, reason),
    })
}
