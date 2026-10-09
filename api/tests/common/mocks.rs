use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use kobload_api::download_clients::{
    DownloadClientCapabilities, DownloadClientError, DownloadClientHandler, DownloadClientResolver,
    PollResult, ScannedFile,
};
use kobload_api::indexers::{
    IndexerCapabilities, IndexerContext, IndexerError, IndexerHandler, IndexerResolver,
    IndexerSearchRequest,
};
use kobload_api::repositories::watch as watch_repo;
use kobload_api::types::{DownloadClientConfig, TestConnectionResult};
use kobload_api::watch::Release;

// =============================================================================
// Mock Download Client
// =============================================================================

pub struct MockDownloadClient {
    pub add_url_results: Mutex<Vec<Result<String, DownloadClientError>>>,
    pub add_bytes_results: Mutex<Vec<Result<String, DownloadClientError>>>,
    pub poll_results: Mutex<Vec<Result<PollResult, DownloadClientError>>>,
}

impl MockDownloadClient {
    pub fn new() -> Self {
        Self {
            add_url_results: Mutex::new(Vec::new()),
            add_bytes_results: Mutex::new(Vec::new()),
            poll_results: Mutex::new(Vec::new()),
        }
    }

    /// Queue a result that will be returned on the next `add_url` call.
    pub fn on_add_url(&self, result: Result<String, DownloadClientError>) {
        self.add_url_results.lock().unwrap().push(result);
    }

    /// Queue a result that will be returned on the next `poll` call.
    pub fn on_poll(&self, result: Result<PollResult, DownloadClientError>) {
        self.poll_results.lock().unwrap().push(result);
    }
}

#[async_trait]
impl DownloadClientHandler for MockDownloadClient {
    fn client_type(&self) -> &'static str {
        "mock"
    }

    fn capabilities(&self) -> DownloadClientCapabilities {
        DownloadClientCapabilities {
            supports_add_url: true,
            supports_add_bytes: true,
        }
    }

    async fn add_url(
        &self,
        _http_client: &reqwest::Client,
        _client: &DownloadClientConfig,
        _url: &str,
    ) -> Result<String, DownloadClientError> {
        let mut results = self.add_url_results.lock().unwrap();
        if results.is_empty() {
            Ok("mock-ext-id".to_string())
        } else {
            results.remove(0)
        }
    }

    async fn add_bytes(
        &self,
        _http_client: &reqwest::Client,
        _client: &DownloadClientConfig,
        _bytes: &[u8],
        _title: &str,
    ) -> Result<String, DownloadClientError> {
        let mut results = self.add_bytes_results.lock().unwrap();
        if results.is_empty() {
            Ok("mock-ext-id".to_string())
        } else {
            results.remove(0)
        }
    }

    async fn poll(
        &self,
        _http_client: &reqwest::Client,
        _client: &DownloadClientConfig,
        _external_id: &str,
    ) -> Result<PollResult, DownloadClientError> {
        let mut results = self.poll_results.lock().unwrap();
        if results.is_empty() {
            Ok(PollResult::completed(vec![]))
        } else {
            results.remove(0)
        }
    }

    async fn remove(
        &self,
        _http_client: &reqwest::Client,
        _client: &DownloadClientConfig,
        _external_id: &str,
        _delete_data: bool,
    ) -> Result<(), DownloadClientError> {
        Ok(())
    }

    async fn test_connection(
        &self,
        _http_client: &reqwest::Client,
        _client: &DownloadClientConfig,
    ) -> TestConnectionResult {
        TestConnectionResult {
            success: true,
            message: "mock connection ok".to_string(),
        }
    }
}

// =============================================================================
// Mock Indexer
// =============================================================================

pub struct MockIndexer {
    pub search_results: Mutex<Vec<Release>>,
    pub rss_results: Mutex<Vec<Release>>,
}

impl MockIndexer {
    pub fn new() -> Self {
        Self {
            search_results: Mutex::new(Vec::new()),
            rss_results: Mutex::new(Vec::new()),
        }
    }

    /// Set the releases that will be returned by `search`.
    pub fn set_search_results(&self, results: Vec<Release>) {
        *self.search_results.lock().unwrap() = results;
    }

    /// Set the releases that will be returned by `rss`.
    pub fn set_rss_results(&self, results: Vec<Release>) {
        *self.rss_results.lock().unwrap() = results;
    }
}

#[async_trait]
impl IndexerHandler for MockIndexer {
    fn handler_type(&self) -> &'static str {
        "mock"
    }

    fn capabilities(&self) -> IndexerCapabilities {
        IndexerCapabilities {
            supports_search: true,
            supports_rss: true,
        }
    }

    async fn search(
        &self,
        _ctx: &IndexerContext<'_>,
        _source: Option<&watch_repo::IndexerSourceRow>,
        _request: IndexerSearchRequest,
    ) -> Result<Vec<Release>, IndexerError> {
        Ok(self.search_results.lock().unwrap().clone())
    }

    async fn rss(
        &self,
        _ctx: &IndexerContext<'_>,
        _source: &watch_repo::IndexerSourceRow,
    ) -> Result<Vec<Release>, IndexerError> {
        Ok(self.rss_results.lock().unwrap().clone())
    }
}

// =============================================================================
// Mock Resolvers
// =============================================================================

/// A resolver that returns a single mock download client for any client_type.
pub struct MockDownloadClientResolver {
    pub client: Arc<MockDownloadClient>,
}

impl MockDownloadClientResolver {
    pub fn new(client: Arc<MockDownloadClient>) -> Self {
        Self { client }
    }
}

impl DownloadClientResolver for MockDownloadClientResolver {
    fn get(&self, _client_type: &str) -> Option<Arc<dyn DownloadClientHandler>> {
        Some(self.client.clone())
    }
}

/// A resolver that returns a single mock indexer for any handler_type.
pub struct MockIndexerResolver {
    pub indexer: Arc<MockIndexer>,
}

impl MockIndexerResolver {
    pub fn new(indexer: Arc<MockIndexer>) -> Self {
        Self { indexer }
    }
}

impl IndexerResolver for MockIndexerResolver {
    fn get(&self, _handler_type: &str) -> Option<Arc<dyn IndexerHandler>> {
        Some(self.indexer.clone())
    }
}
