use std::sync::Arc;

use async_trait::async_trait;
use usenet_proto::{ImportRequest, ImportResponse};

use super::provider::DebridProvider;
use super::types::{
    CachedAvailability, DebridAccountStatus, DebridError, DebridProviderType, DebridTorrent,
    UnrestrictedLink,
};
use crate::usenet::{EngineError, UsenetEngine};

/// Streams from our own usenet engine. It holds no torrents, and needs no API key: the
/// engine is reached with the control key kobload was started with.
pub struct UsenetProvider {
    engine: Option<Arc<UsenetEngine>>,
}

impl UsenetProvider {
    pub fn new(engine: Option<Arc<UsenetEngine>>) -> Self {
        Self { engine }
    }

    fn engine(&self) -> Result<&UsenetEngine, DebridError> {
        self.engine.as_deref().ok_or_else(|| {
            DebridError::Other(
                "The usenet engine is not configured (USENET_ENGINE_URL, USENET_STREAM_BASE_URL, \
                 USENET_CONTROL_KEY, USENET_STREAM_SECRET)"
                    .to_string(),
            )
        })
    }
}

impl From<EngineError> for DebridError {
    fn from(e: EngineError) -> Self {
        match e {
            EngineError::NotCached | EngineError::UnknownJob(_) => Self::NotFound,
            EngineError::Rejected(reason) => {
                Self::Status(reqwest::StatusCode::UNPROCESSABLE_ENTITY, reason)
            }
            EngineError::Unavailable(reason) => {
                Self::Status(reqwest::StatusCode::SERVICE_UNAVAILABLE, reason)
            }
            EngineError::Status(code, reason) => Self::Status(
                reqwest::StatusCode::from_u16(code).unwrap_or(reqwest::StatusCode::BAD_GATEWAY),
                reason,
            ),
            EngineError::Request(e) => Self::Request(e),
        }
    }
}

#[async_trait]
impl DebridProvider for UsenetProvider {
    fn provider_type(&self) -> DebridProviderType {
        DebridProviderType::Usenet
    }

    async fn usenet_import(&self, request: &ImportRequest) -> Result<ImportResponse, DebridError> {
        Ok(self.engine()?.import(request).await?)
    }

    async fn usenet_release(&self, release_key: &str) -> Result<ImportResponse, DebridError> {
        Ok(self.engine()?.release(release_key).await?)
    }

    async fn check_availability(
        &self,
        _http_client: &reqwest::Client,
        _api_key: &str,
        _hashes: &[&str],
    ) -> Result<Vec<CachedAvailability>, DebridError> {
        Ok(Vec::new())
    }

    async fn add_magnet(
        &self,
        _http_client: &reqwest::Client,
        _api_key: &str,
        _magnet: &str,
    ) -> Result<String, DebridError> {
        Err(DebridError::UnsupportedOperation)
    }

    async fn select_files(
        &self,
        _http_client: &reqwest::Client,
        _api_key: &str,
        _torrent_id: &str,
        _file_ids: &[i32],
    ) -> Result<(), DebridError> {
        Err(DebridError::UnsupportedOperation)
    }

    async fn get_torrent(
        &self,
        _http_client: &reqwest::Client,
        _api_key: &str,
        _torrent_id: &str,
    ) -> Result<DebridTorrent, DebridError> {
        Err(DebridError::UnsupportedOperation)
    }

    async fn unrestrict_link(
        &self,
        _http_client: &reqwest::Client,
        _api_key: &str,
        _link: &str,
    ) -> Result<UnrestrictedLink, DebridError> {
        Err(DebridError::UnsupportedOperation)
    }

    async fn get_account_status(
        &self,
        _http_client: &reqwest::Client,
        _api_key: &str,
    ) -> Result<DebridAccountStatus, DebridError> {
        self.engine()?.health().await?;
        Ok(DebridAccountStatus {
            provider: DebridProviderType::Usenet.display_name().to_string(),
            username: None,
            expiration: None,
            premium: true,
            remaining_traffic: None,
        })
    }
}
