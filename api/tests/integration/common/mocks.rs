use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use kobload_api::download_clients::{
    DownloadClientCapabilities, DownloadClientError, DownloadClientHandler, DownloadClientResolver,
    NewDownload, PollResult, ScannedFile,
};
use kobload_api::indexers::{
    IndexerCapabilities, IndexerContext, IndexerError, IndexerHandler, IndexerResolver,
    IndexerSearchRequest,
};
use kobload_api::repositories::watch as watch_repo;
use kobload_api::types::{DownloadClientConfig, TestConnectionResult};
use kobload_api::watch::Release;
use uuid::Uuid;

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
        _download: &NewDownload<'_>,
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
    season_only_results: Mutex<Option<Vec<Release>>>,
    per_episode_results: Mutex<Option<Vec<Release>>>,
    search_delay: Mutex<Duration>,
    query_delays: Mutex<Option<(Duration, Duration)>>,
    source_delays: Mutex<HashMap<Uuid, Duration>>,
    source_results: Mutex<HashMap<Uuid, Vec<Release>>>,
    source_errors: Mutex<HashMap<Uuid, axum::http::StatusCode>>,
    searches: AtomicUsize,
    searched_episodes: Mutex<BTreeSet<String>>,
}

impl MockIndexer {
    pub fn new() -> Self {
        Self {
            search_results: Mutex::new(Vec::new()),
            rss_results: Mutex::new(Vec::new()),
            season_only_results: Mutex::new(None),
            per_episode_results: Mutex::new(None),
            search_delay: Mutex::new(Duration::ZERO),
            query_delays: Mutex::new(None),
            source_delays: Mutex::new(HashMap::new()),
            source_results: Mutex::new(HashMap::new()),
            source_errors: Mutex::new(HashMap::new()),
            searches: AtomicUsize::new(0),
            searched_episodes: Mutex::new(BTreeSet::new()),
        }
    }

    pub fn set_search_delay(&self, delay: Duration) {
        *self.search_delay.lock().unwrap() = delay;
    }

    pub fn set_query_delays(&self, episode: Duration, season: Duration) {
        *self.query_delays.lock().unwrap() = Some((episode, season));
    }

    pub fn set_source_delay(&self, source: Uuid, delay: Duration) {
        self.source_delays.lock().unwrap().insert(source, delay);
    }

    pub fn set_source_results(&self, source: Uuid, results: Vec<Release>) {
        self.source_results.lock().unwrap().insert(source, results);
    }

    pub fn set_source_error(&self, source: Uuid, status: axum::http::StatusCode) {
        self.source_errors.lock().unwrap().insert(source, status);
    }

    fn delay_for(&self, source: Option<Uuid>, request: &IndexerSearchRequest) -> Duration {
        if let Some(delay) =
            source.and_then(|id| self.source_delays.lock().unwrap().get(&id).copied())
        {
            return delay;
        }
        match *self.query_delays.lock().unwrap() {
            Some((episode, _)) if request.episode.is_some() => episode,
            Some((_, season)) => season,
            None => *self.search_delay.lock().unwrap(),
        }
    }

    pub fn take_searched_episodes(&self) -> BTreeSet<String> {
        std::mem::take(&mut self.searched_episodes.lock().unwrap())
    }

    pub fn searches(&self) -> usize {
        self.searches.load(Ordering::SeqCst)
    }

    /// Set the releases that will be returned by `search`.
    pub fn set_search_results(&self, results: Vec<Release>) {
        *self.search_results.lock().unwrap() = results;
    }

    /// A newznab `ep` search never lists season packs; only a season-only search does.
    pub fn set_season_only_results(&self, results: Vec<Release>) {
        *self.season_only_results.lock().unwrap() = Some(results);
    }

    /// A text query naming an episode (`S02E03`) finds only that episode's
    /// releases from these; any other query gets the plain search results.
    pub fn set_per_episode_results(&self, results: Vec<Release>) {
        *self.per_episode_results.lock().unwrap() = Some(results);
    }

    /// Set the releases that will be returned by `rss`.
    pub fn set_rss_results(&self, results: Vec<Release>) {
        *self.rss_results.lock().unwrap() = results;
    }
}

fn is_episode_token(word: &str) -> bool {
    let bytes = word.as_bytes();
    bytes.len() == 6
        && bytes[0] == b'S'
        && bytes[3] == b'E'
        && [1, 2, 4, 5].iter().all(|&i| bytes[i].is_ascii_digit())
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
        source: Option<&watch_repo::IndexerSourceRow>,
        request: IndexerSearchRequest,
    ) -> Result<Vec<Release>, IndexerError> {
        self.searches.fetch_add(1, Ordering::SeqCst);
        if let Some(token) = request
            .query
            .split_whitespace()
            .find(|w| is_episode_token(w))
        {
            self.searched_episodes
                .lock()
                .unwrap()
                .insert(token.to_string());
        }
        let source_id = source.map(|s| s.id);
        let delay = self.delay_for(source_id, &request);
        tokio::time::sleep(delay).await;
        if let Some(status) =
            source_id.and_then(|id| self.source_errors.lock().unwrap().get(&id).copied())
        {
            return Err(IndexerError::new(status, "mock indexer failure"));
        }
        if let Some(results) =
            source_id.and_then(|id| self.source_results.lock().unwrap().get(&id).cloned())
        {
            return Ok(results);
        }
        if let Some(per_episode) = self.per_episode_results.lock().unwrap().as_ref()
            && let Some(token) = request
                .query
                .split_whitespace()
                .find(|w| is_episode_token(w))
        {
            return Ok(per_episode
                .iter()
                .filter(|r| r.title.contains(token))
                .cloned()
                .collect());
        }
        if request.episode.is_none()
            && let Some(season_only) = self.season_only_results.lock().unwrap().clone()
        {
            return Ok(season_only);
        }
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
