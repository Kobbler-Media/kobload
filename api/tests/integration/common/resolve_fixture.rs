use std::sync::Arc;
use std::time::Duration;

use kobload_api::debrid::DebridProviderResolver;
use kobload_api::services::stream::warm_cache::warm_top_results;
use kobload_api::services::stream::{StreamResolveContext, stream_resolve};
use nats_jobs::types::{
    StreamFile, StreamOption, StreamPriority, StreamResolveRequest, StreamResolveResponse,
    StreamSourceType,
};
use uuid::Uuid;

use super::TestServer;
use super::debrid_mock::{MockDebrid, MockRelease};

pub struct Fixture {
    pub server: TestServer,
    pub provider_id: Uuid,
    pub mock: Arc<MockDebrid>,
    pub resolver: Arc<dyn DebridProviderResolver>,
}

impl Fixture {
    pub async fn new(latency_ms: u64, releases: &[(&str, MockRelease)]) -> Self {
        Self::with_mock(MockDebrid::new(Duration::from_millis(latency_ms)).with_releases(releases))
            .await
    }

    pub async fn with_mock(mock: Arc<MockDebrid>) -> Self {
        let server = TestServer::start().await;
        let provider_id: Uuid = sqlx::query_scalar(
            "INSERT INTO debrid_providers (name, provider_type, api_key)
             VALUES ('mock', 'real_debrid', $1) RETURNING id",
        )
        .bind(Uuid::new_v4().to_string())
        .fetch_one(&server.pool)
        .await
        .expect("create provider");
        let resolver = mock.resolver();
        Self {
            server,
            provider_id,
            mock,
            resolver,
        }
    }

    pub async fn resolve(&self, hash: &str) -> Result<StreamResolveResponse, String> {
        self.resolve_with(hash, None, None).await
    }

    pub async fn resolve_file(
        &self,
        hash: &str,
        file_id: i32,
    ) -> Result<StreamResolveResponse, String> {
        self.resolve_with(hash, Some(file_id), None).await
    }

    pub async fn resolve_with(
        &self,
        hash: &str,
        file_id: Option<i32>,
        priority: Option<StreamPriority>,
    ) -> Result<StreamResolveResponse, String> {
        let ctx = StreamResolveContext {
            pool: &self.server.pool,
            http_client: &self.server.app_state.http_client,
            debrid_providers: self.resolver.as_ref(),
        };
        stream_resolve(
            &ctx,
            &StreamResolveRequest {
                debrid_provider_id: self.provider_id,
                info_hash: Some(hash.to_string()),
                magnet_url: None,
                nzb_url: None,
                file_id,
                priority,
            },
        )
        .await
        .map_err(|e| e.to_string())
    }

    pub fn warm(&self, hashes: &[&str]) -> tokio::task::JoinHandle<()> {
        self.warm_options(hashes.iter().map(|h| self.option(h)).collect())
    }

    pub fn warm_options(&self, options: Vec<StreamOption>) -> tokio::task::JoinHandle<()> {
        tokio::spawn(warm_top_results(
            self.server.pool.clone(),
            self.server.app_state.http_client.clone(),
            self.resolver.clone(),
            self.server.app_state.download_events.clone(),
            options,
        ))
    }

    pub fn option(&self, hash: &str) -> StreamOption {
        StreamOption {
            id: format!("torrent_{hash}_{}", self.provider_id),
            release_title: format!("Show.S01E01.1080p.BluRay.x264-{}", &hash[..4]),
            size: 2_000_000_000,
            indexer: "torrentio".to_string(),
            info_hash: Some(hash.to_string()),
            nzb_url: None,
            magnet_url: None,
            quality: None,
            total_score: 1.0,
            match_score: 1.0,
            quality_score: 0.0,
            debrid_provider: "real_debrid".to_string(),
            debrid_provider_id: self.provider_id,
            cached: false,
            source_type: StreamSourceType::Torrent,
            files: Vec::new(),
            publish_date: None,
            binge_group: None,
            playback: None,
            availability: None,
            audio_languages: Vec::new(),
        }
    }

    pub fn file_option(&self, hash: &str, file_id: i32, path: &str) -> StreamOption {
        let mut option = self.option(hash);
        option.id = format!("torrent_{hash}_f{file_id}_{}", self.provider_id);
        option.files = vec![StreamFile {
            id: file_id,
            path: path.to_string(),
            size: 2_000_000_000,
        }];
        option
    }
}
