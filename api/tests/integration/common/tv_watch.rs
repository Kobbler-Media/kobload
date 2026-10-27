use std::sync::Arc;

use serde_json::json;
use uuid::Uuid;

use super::{
    MockDownloadClient, MockDownloadClientResolver, MockIndexer, MockIndexerResolver, TestServer,
    TestServerOptions,
};
use kobload_api::debrid::DebridProviderResolver;
use kobload_api::repositories::{clients as clients_repo, watch as watch_repo};
use kobload_api::services::stream::{StreamSearchContext, stream_search};
use kobload_api::watch::Release;
use nats_jobs::types::{StreamOption, StreamSearchRequest};

pub struct TvWatch {
    pub server: TestServer,
    pub indexer: Arc<MockIndexer>,
    pub source: watch_repo::IndexerSourceRow,
    pub media_type_id: Uuid,
}

impl TvWatch {
    pub async fn start() -> Self {
        Self::start_with_debrid(Arc::new(kobload_api::debrid::DefaultDebridProviderResolver)).await
    }

    pub async fn start_with_debrid(debrid: Arc<dyn DebridProviderResolver>) -> Self {
        let indexer = Arc::new(MockIndexer::new());
        let options = TestServerOptions {
            download_clients: Arc::new(MockDownloadClientResolver::new(Arc::new(
                MockDownloadClient::new(),
            ))),
            indexers: Arc::new(MockIndexerResolver::new(indexer.clone())),
        };
        let server = TestServer::start_inner(options, debrid).await;
        let pool = &server.pool;

        clients_repo::create_client(
            pool,
            clients_repo::DownloadClientInput {
                name: "Mock Usenet Client".to_string(),
                client_type: "mock".to_string(),
                protocol: "usenet".to_string(),
                base_url: "http://localhost:9999".to_string(),
                is_default: true,
                enabled: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let source_id = watch_repo::create_indexer_source(
            pool,
            watch_repo::IndexerSourceInput {
                debrid_mode: None,
                name: "Mock Newznab".to_string(),
                handler_type: "newznab".to_string(),
                handler_config: json!({ "base_url": "http://localhost:9998" }),
                category_ids: None,
                media_type_ids: vec![],
                enabled: true,
                priority: 0,
                seed_ratio: None,
                seed_time_minutes: None,
                search_mode: None,
                auto_throttle: None,
                api_limit_daily: None,
            },
        )
        .await
        .unwrap();
        let source = watch_repo::get_indexer_source_by_id(pool, source_id)
            .await
            .unwrap()
            .unwrap();
        let (media_type_id,): (Uuid,) =
            sqlx::query_as("SELECT id FROM media_type_config WHERE key = 'tv' LIMIT 1")
                .fetch_one(pool)
                .await
                .unwrap();

        Self {
            server,
            indexer,
            source,
            media_type_id,
        }
    }

    pub async fn pattern(&self, want: &nats_jobs::MediaWant) -> watch_repo::WatchPatternRow {
        let pool = &self.server.pool;
        let pattern_id = watch_repo::create_pattern(
            pool,
            "test",
            &Uuid::now_v7().to_string(),
            watch_repo::WatchPatternInput {
                callback_url: "http://localhost:1111/callback".to_string(),
                patterns: vec![],
                media_want: Some(serde_json::to_value(want).unwrap()),
                match_targets: None,
                match_mode: Some("fuzzy".to_string()),
                min_score: Some(0.5),
                media_type_id: Some(self.media_type_id),
                auto_download: true,
                metadata: None,
                enabled: true,
                upgrade_allowed: true,
                search_not_before: None,
            },
        )
        .await
        .unwrap();
        watch_repo::get_pattern_by_id(pool, pattern_id)
            .await
            .unwrap()
            .unwrap()
    }

    /// Stream-searches through a debrid-enabled source, returning the offered
    /// release titles in ranked order.
    pub async fn stream_titles(&self, want: &nats_jobs::MediaWant) -> Vec<String> {
        self.stream_options(want)
            .await
            .into_iter()
            .map(|o| o.release_title)
            .collect()
    }

    pub async fn debrid_provider(&self) -> Uuid {
        let existing: Option<Uuid> = sqlx::query_scalar("SELECT id FROM debrid_providers LIMIT 1")
            .fetch_optional(&self.server.pool)
            .await
            .unwrap();
        match existing {
            Some(id) => id,
            None => {
                super::stream_fixtures::create_provider(&self.server.pool, "real_debrid")
                    .await
                    .id
            }
        }
    }

    pub async fn stream_options(&self, want: &nats_jobs::MediaWant) -> Vec<StreamOption> {
        let pool = &self.server.pool;
        self.debrid_provider().await;
        sqlx::query("UPDATE indexer_sources SET debrid_mode = 'both' WHERE id = $1")
            .bind(self.source.id)
            .execute(pool)
            .await
            .unwrap();
        let state = &self.server.app_state;
        let ctx = StreamSearchContext {
            pool,
            http_client: &state.http_client,
            indexers: state.indexers.as_ref(),
            debrid_providers: state.debrid_providers.as_ref(),
            definitions: &state.definitions,
            xem_cache: state.xem_cache.as_ref(),
            scene_mappings: state.scene_mappings.as_ref(),
            trash_filters: state.trash_filters.as_ref(),
            download_events: &state.download_events,
            debrid_providers_arc: state.debrid_providers.clone(),
        };
        let request = StreamSearchRequest {
            media_want: Some(want.clone()),
            query: String::new(),
            media_type: Some("tv".to_string()),
            quality_profile_id: None,
            limit: None,
            include_uncached: Some(true),
            prefer: None,
            audio_languages: Vec::new(),
        };
        stream_search(&ctx, &request).await.unwrap().options
    }

    pub async fn grabbed(&self, pattern_id: Uuid) -> Vec<String> {
        watch_repo::list_matches_by_pattern(&self.server.pool, pattern_id, 10)
            .await
            .unwrap()
            .into_iter()
            .filter(|m| m.action == "downloaded")
            .map(|m| m.release_guid)
            .collect()
    }
}

pub fn episode_want(
    show: &str,
    season: &str,
    episode: &str,
    tvdb_id: Option<i64>,
) -> nats_jobs::MediaWant {
    nats_jobs::MediaWant::TvEpisode {
        show_name: show.to_string(),
        season: season.to_string(),
        episode: episode.to_string(),
        year: None,
        air_date: None,
        identifiers: tvdb_id.map(|id| nats_jobs::TvIdentifiers {
            tmdb_id: None,
            tvdb_id: Some(id.to_string()),
            imdb_id: None,
        }),
        alternate_titles: None,
        episode_title: None,
        quality_profile_id: None,
        quality_overrides: None,
    }
}

pub fn usenet_release(guid: &str, title: &str) -> Release {
    serde_json::from_value(json!({
        "guid": guid,
        "title": title,
        "download_url": format!("http://example.com/download/{guid}"),
        "size": 1_500_000_000i64,
        "publish_date": "2026-10-01",
        "category_ids": [5000],
        "indexer": "mock-indexer",
        "protocol": "usenet",
        "raw_data": {},
    }))
    .unwrap()
}

pub fn torrent_release(guid: &str, title: &str, hash_char: char) -> Release {
    let mut release = usenet_release(guid, title);
    let info_hash = super::stream_fixtures::hash(hash_char);
    release.protocol = "torrent".to_string();
    release.download_url = None;
    release.magnet_url = Some(format!("magnet:?xt=urn:btih:{info_hash}"));
    release.info_hash = Some(info_hash);
    release
}
