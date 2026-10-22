//! RSS scoring must apply the XEM scene-numbering bonus that search applies.
//!
//! The want is TVDB S01E13, which the scene numbers S02E01. Scene S01E13 is a
//! different episode (TVDB S01E14), so a release labelled S01E13 is a
//! numbering collision and XEM withholds its +0.5 episode bonus. Without XEM
//! the collision keeps the bonus and outscores an unambiguous 2160p release
//! numbered 13 absolute, and RSS grabs the 720p collision.

use crate::common;

use std::sync::Arc;

use serde_json::json;
use time::OffsetDateTime;
use uuid::Uuid;

use common::{
    MockDownloadClient, MockDownloadClientResolver, MockIndexer, MockIndexerResolver, TestServer,
    TestServerOptions,
};
use kobload_api::repositories::{clients as clients_repo, watch as watch_repo, xem_cache};
use kobload_api::watch::{PreparedPattern, Release, process_releases, rss_outcome};

const TVDB_ID: i64 = 424_242;
const ABSOLUTE_GUID: &str = "xem-absolute-13";
const COLLISION_GUID: &str = "xem-collision-s01e13";

fn show_map() -> serde_json::Value {
    let entry = |scene: (i32, i32), tvdb: (i32, i32)| {
        json!({
            "scene": {"season": scene.0, "episode": scene.1, "absolute": null},
            "tvdb": {"season": tvdb.0, "episode": tvdb.1, "absolute": null},
        })
    };
    json!({"entries": [entry((2, 1), (1, 13)), entry((1, 13), (1, 14))]})
}

fn media_want() -> nats_jobs::MediaWant {
    nats_jobs::MediaWant::TvEpisode {
        show_name: "Kitsune Days".to_string(),
        season: "1".to_string(),
        episode: "13".to_string(),
        year: None,
        air_date: None,
        identifiers: Some(nats_jobs::TvIdentifiers {
            tmdb_id: None,
            tvdb_id: Some(TVDB_ID.to_string()),
            imdb_id: None,
        }),
        alternate_titles: None,
        episode_title: None,
        quality_profile_id: None,
        quality_overrides: None,
    }
}

fn release(guid: &str, title: &str) -> Release {
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

fn releases() -> Vec<Release> {
    vec![
        release(COLLISION_GUID, "Kitsune.Days.S01E13.720p.HDTV.x264-GRP"),
        release(
            ABSOLUTE_GUID,
            "[Erai-raws] Kitsune Days - 13 [2160p][Multiple Subtitle]",
        ),
    ]
}

#[tokio::test]
async fn rss_poll_skips_the_numbering_collision() {
    let options = TestServerOptions {
        download_clients: Arc::new(MockDownloadClientResolver::new(Arc::new(
            MockDownloadClient::new(),
        ))),
        indexers: Arc::new(MockIndexerResolver::new(Arc::new(MockIndexer::new()))),
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
    let (media_type_id,): (Uuid,) =
        sqlx::query_as("SELECT id FROM media_type_config WHERE key = 'tv' LIMIT 1")
            .fetch_one(pool)
            .await
            .unwrap();
    xem_cache::put(pool, TVDB_ID, &show_map()).await.unwrap();

    let pattern_id = watch_repo::create_pattern(
        pool,
        "test",
        "kitsune-days-s01e13",
        watch_repo::WatchPatternInput {
            callback_url: "http://localhost:1111/callback".to_string(),
            patterns: vec![],
            media_want: Some(serde_json::to_value(media_want()).unwrap()),
            match_targets: None,
            match_mode: Some("fuzzy".to_string()),
            min_score: Some(0.5),
            media_type_id: Some(media_type_id),
            auto_download: true,
            metadata: None,
            enabled: true,
            upgrade_allowed: true,
            search_not_before: None,
        },
    )
    .await
    .unwrap();
    let pattern = watch_repo::get_pattern_by_id(pool, pattern_id)
        .await
        .unwrap()
        .unwrap();
    let source = watch_repo::get_indexer_source_by_id(pool, source_id)
        .await
        .unwrap()
        .unwrap();

    process_releases(&server.app_state, &source, &releases(), &[pattern]).await;

    let matches = watch_repo::list_matches_by_pattern(pool, pattern_id, 10)
        .await
        .unwrap();
    let grabbed: Vec<&str> = matches.iter().map(|m| m.release_guid.as_str()).collect();
    assert_eq!(grabbed, [ABSOLUTE_GUID]);

    server.cleanup().await;
}

/// A captured pattern set carries its XEM map, so a replay scores the same way.
#[test]
fn captured_patterns_replay_with_the_xem_bonus() {
    let now = serde_json::to_value(OffsetDateTime::now_utc()).unwrap();
    let mw = serde_json::to_value(media_want()).unwrap();
    let target = |id: &str, target: &str| json!({"id": id, "target": target, "min_score": null});
    let prepared: Vec<PreparedPattern> = serde_json::from_value(json!([{
        "pattern": {
            "id": Uuid::nil(),
            "source_system": "test",
            "source_id": "kitsune-days-s01e13",
            "callback_url": "http://localhost/callback",
            "patterns": [],
            "media_want": mw,
            "match_targets": null,
            "match_mode": null,
            "min_score": 0.5,
            "media_type_id": null,
            "auto_download": true,
            "metadata": null,
            "enabled": true,
            "upgrade_allowed": true,
            "upgrade_until": null,
            "created_at": now,
            "updated_at": now,
            "last_backlog_search_at": null,
            "backlog_search_interval_hours": null,
            "excluded_releases": null,
            "search_not_before": null,
        },
        "quality_profile": null,
        "match_config": {
            "patterns": ["Kitsune Days S01E13"],
            "match_targets": [
                target("tvdb", "Kitsune Days S01E13"),
                target("scene", "Kitsune Days S02E01"),
            ],
            "match_mode": "fuzzy",
            "min_score": 0.5
        },
        "categories": [],
        "matched_guids": [],
        "_is_season": false,
        "parsed_media_want": mw,
        "episodes_needed": null,
        "preferred_protocol": null,
        "xem_show_map": show_map(),
    }]))
    .unwrap();
    let releases: Vec<Arc<Release>> = releases().into_iter().map(Arc::new).collect();

    let (outcome, _) = rss_outcome(
        &prepared,
        &releases,
        Uuid::nil(),
        None,
        OffsetDateTime::now_utc(),
        false,
        None,
    );
    let picked: Vec<&str> = outcome.candidates.iter().map(|c| c.guid.as_str()).collect();
    assert_eq!(picked, [ABSOLUTE_GUID]);
}
