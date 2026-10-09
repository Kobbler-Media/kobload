use std::sync::Arc;

use async_trait::async_trait;
use sqlx::PgPool;

use crate::debrid::DebridProviderResolver;
use crate::types::{DownloadClientConfig, TestConnectionResult};

use super::debrid::DebridHandler;
use super::{
    DownloadClientError, PollResult, add_sabnzbd, add_transmission_bytes, add_transmission_url,
    poll_sabnzbd, poll_transmission, remove_sabnzbd, remove_transmission,
};

#[derive(Debug, Clone, Copy)]
pub struct DownloadClientCapabilities {
    pub supports_add_url: bool,
    pub supports_add_bytes: bool,
}

#[async_trait]
pub trait DownloadClientHandler: Send + Sync {
    fn client_type(&self) -> &'static str;
    fn capabilities(&self) -> DownloadClientCapabilities;

    async fn add_url(
        &self,
        http_client: &reqwest::Client,
        client: &DownloadClientConfig,
        url: &str,
    ) -> Result<String, DownloadClientError>;

    async fn add_bytes(
        &self,
        http_client: &reqwest::Client,
        client: &DownloadClientConfig,
        bytes: &[u8],
        title: &str,
    ) -> Result<String, DownloadClientError>;

    async fn poll(
        &self,
        http_client: &reqwest::Client,
        client: &DownloadClientConfig,
        external_id: &str,
    ) -> Result<PollResult, DownloadClientError>;

    async fn remove(
        &self,
        http_client: &reqwest::Client,
        client: &DownloadClientConfig,
        external_id: &str,
        delete_data: bool,
    ) -> Result<(), DownloadClientError>;

    async fn test_connection(
        &self,
        http_client: &reqwest::Client,
        client: &DownloadClientConfig,
    ) -> TestConnectionResult;
}

/// Trait for resolving download client handlers by type.
/// Inject via AppState to allow test mocks.
pub trait DownloadClientResolver: Send + Sync {
    fn get(&self, client_type: &str) -> Option<Arc<dyn DownloadClientHandler>>;
}

/// Default resolver that delegates to the static registry.
/// Holds shared state needed by stateful handlers (e.g., debrid).
pub struct DefaultDownloadClientResolver {
    pool: PgPool,
    http_client: reqwest::Client,
    debrid_providers: Arc<dyn DebridProviderResolver>,
}

impl DefaultDownloadClientResolver {
    pub fn new(
        pool: PgPool,
        http_client: reqwest::Client,
        debrid_providers: Arc<dyn DebridProviderResolver>,
    ) -> Self {
        Self {
            pool,
            http_client,
            debrid_providers,
        }
    }
}

impl DownloadClientResolver for DefaultDownloadClientResolver {
    fn get(&self, client_type: &str) -> Option<Arc<dyn DownloadClientHandler>> {
        match client_type {
            "transmission" => Some(Arc::new(TransmissionHandler)),
            "sabnzbd" => Some(Arc::new(SabnzbdHandler)),
            "debrid" => Some(Arc::new(DebridHandler::new(
                self.pool.clone(),
                self.http_client.clone(),
                self.debrid_providers.clone(),
            ))),
            _ => None,
        }
    }
}

struct TransmissionHandler;
struct SabnzbdHandler;

#[async_trait]
impl DownloadClientHandler for TransmissionHandler {
    fn client_type(&self) -> &'static str {
        "transmission"
    }

    fn capabilities(&self) -> DownloadClientCapabilities {
        DownloadClientCapabilities {
            supports_add_url: true,
            supports_add_bytes: true,
        }
    }

    async fn add_url(
        &self,
        http_client: &reqwest::Client,
        client: &DownloadClientConfig,
        url: &str,
    ) -> Result<String, DownloadClientError> {
        add_transmission_url(http_client, client, url).await
    }

    async fn add_bytes(
        &self,
        http_client: &reqwest::Client,
        client: &DownloadClientConfig,
        bytes: &[u8],
        _title: &str,
    ) -> Result<String, DownloadClientError> {
        add_transmission_bytes(http_client, client, bytes).await
    }

    async fn poll(
        &self,
        http_client: &reqwest::Client,
        client: &DownloadClientConfig,
        external_id: &str,
    ) -> Result<PollResult, DownloadClientError> {
        poll_transmission(http_client, client, external_id).await
    }

    async fn remove(
        &self,
        http_client: &reqwest::Client,
        client: &DownloadClientConfig,
        external_id: &str,
        delete_data: bool,
    ) -> Result<(), DownloadClientError> {
        remove_transmission(http_client, client, external_id, delete_data).await
    }

    async fn test_connection(
        &self,
        http_client: &reqwest::Client,
        client: &DownloadClientConfig,
    ) -> TestConnectionResult {
        crate::download_clients::transmission::test_transmission(http_client, client).await
    }
}

#[async_trait]
impl DownloadClientHandler for SabnzbdHandler {
    fn client_type(&self) -> &'static str {
        "sabnzbd"
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
            "sabnzbd requires an NZB file, not a URL".to_string(),
        ))
    }

    async fn add_bytes(
        &self,
        http_client: &reqwest::Client,
        client: &DownloadClientConfig,
        bytes: &[u8],
        title: &str,
    ) -> Result<String, DownloadClientError> {
        add_sabnzbd(http_client, client, bytes, title).await
    }

    async fn poll(
        &self,
        http_client: &reqwest::Client,
        client: &DownloadClientConfig,
        external_id: &str,
    ) -> Result<PollResult, DownloadClientError> {
        poll_sabnzbd(http_client, client, external_id).await
    }

    async fn remove(
        &self,
        http_client: &reqwest::Client,
        client: &DownloadClientConfig,
        external_id: &str,
        delete_data: bool,
    ) -> Result<(), DownloadClientError> {
        remove_sabnzbd(http_client, client, external_id, delete_data).await
    }

    async fn test_connection(
        &self,
        http_client: &reqwest::Client,
        client: &DownloadClientConfig,
    ) -> TestConnectionResult {
        crate::download_clients::sabnzbd::test_sabnzbd(http_client, client).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::debrid::DefaultDebridProviderResolver;

    fn test_resolver() -> DefaultDownloadClientResolver {
        // Use a dummy pool — tests only check handler lookup, not DB access
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let pool = rt.block_on(async {
            sqlx::PgPool::connect_lazy("postgres://localhost/nonexistent").unwrap()
        });
        let http = reqwest::Client::new();
        let debrid = Arc::new(DefaultDebridProviderResolver);
        DefaultDownloadClientResolver::new(pool, http, debrid)
    }

    #[test]
    fn test_resolver_lookup() {
        let resolver = test_resolver();
        assert!(resolver.get("transmission").is_some());
        assert!(resolver.get("sabnzbd").is_some());
        assert!(resolver.get("debrid").is_some());
        assert!(resolver.get("unknown").is_none());
    }

    #[test]
    fn test_capabilities() {
        let resolver = test_resolver();
        let transmission = resolver.get("transmission").expect("handler");
        let caps = transmission.capabilities();
        assert!(caps.supports_add_url);
        assert!(caps.supports_add_bytes);

        let sabnzbd = resolver.get("sabnzbd").expect("handler");
        let caps = sabnzbd.capabilities();
        assert!(!caps.supports_add_url);
        assert!(caps.supports_add_bytes);

        let debrid = resolver.get("debrid").expect("handler");
        let caps = debrid.capabilities();
        assert!(caps.supports_add_url);
        assert!(!caps.supports_add_bytes);
    }
}
