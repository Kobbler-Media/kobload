use std::sync::Arc;

use serde_json::json;
use uuid::Uuid;

use super::{
    MockDownloadClient, MockDownloadClientResolver, MockIndexer, MockIndexerResolver, TestServer,
    TestServerOptions,
};
use kobload_api::repositories::{clients as clients_repo, watch as watch_repo};
use kobload_api::watch::Release;

pub struct TvWatch {
    pub server: TestServer,
    pub indexer: Arc<MockIndexer>,
    pub source: watch_repo::IndexerSourceRow,
    pub media_type_id: Uuid,
}

impl TvWatch {
    pub async fn start() -> Self {
        let indexer = Arc::new(MockIndexer::new());
        let options = TestServerOptions {
            download_clients: Arc::new(MockDownloadClientResolver::new(Arc::new(
                MockDownloadClient::new(),
            ))),
            indexers: Arc::new(MockIndexerResolver::new(indexer.clone())),
        };
        let server = TestServer::start_with_options(options).await;
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
