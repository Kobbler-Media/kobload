//! Tests for the active search flow in Kobload.
//!
//! These tests exercise the search path of want processing: pattern cascade,
//! match target scoring, early exit, and multi-target downloads. They complement
//! the basic want tests in e2e_want_test.rs by focusing on the search/scoring logic.

use crate::common;

use std::sync::Arc;

use serde_json::{Value, json};
use uuid::Uuid;

use common::{
    MockDownloadClient, MockDownloadClientResolver, MockIndexer, MockIndexerResolver, TestServer,
    TestServerOptions,
};
use kobload_api::repositories::{clients as clients_repo, watch as watch_repo};
use kobload_api::watch::Release;

/// Helper: start a test server with mock resolvers.
async fn start_mock_server() -> (TestServer, Arc<MockDownloadClient>, Arc<MockIndexer>) {
    let mock_dl = Arc::new(MockDownloadClient::new());
    let mock_idx = Arc::new(MockIndexer::new());
    let options = TestServerOptions {
        download_clients: Arc::new(MockDownloadClientResolver::new(mock_dl.clone())),
        indexers: Arc::new(MockIndexerResolver::new(mock_idx.clone())),
    };
    let server = TestServer::start_with_options(options).await;
    (server, mock_dl, mock_idx)
}

async fn seed_download_client(pool: &sqlx::PgPool) -> Uuid {
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
    .expect("create download client")
}

async fn seed_indexer_source(pool: &sqlx::PgPool) -> Uuid {
    watch_repo::create_indexer_source(
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
    .expect("create indexer source")
}

fn make_release(title: &str, guid: &str, source_id: Option<Uuid>) -> Release {
    Release {
        guid: guid.to_string(),
        title: title.to_string(),
        file_name: None,
        download_url: Some("http://example.com/download/123".to_string()),
        magnet_url: None,
        info_url: None,
        size: 1_048_576,
        publish_date: "2025-01-01".to_string(),
        category_ids: vec![7000],
        indexer: "mock-indexer".to_string(),
        protocol: "usenet".to_string(),
        info_hash: None,
        files: None,
        seeders: None,
        peers: None,
        leechers: None,
        grabs: None,
        votes: None,
        indexer_id: None,
        indexer_source_id: source_id,
        raw_data: json!({}),
        external_ids: vec![],
        media: None,
    }
}

// =============================================================================
// Pattern Cascade Tests
// =============================================================================

/// When multiple patterns are provided, the search cascades through them.
/// If the first pattern yields a high-scoring match (>= early_exit_score),
/// subsequent patterns are skipped.
#[tokio::test]
async fn test_search_cascade_early_exit() {
    let (server, _mock_dl, mock_idx) = start_mock_server().await;
    let pool = &server.pool;

    seed_download_client(pool).await;
    let source_id = seed_indexer_source(pool).await;

    // Mock returns a release matching the first pattern very well.
    let release = make_release(
        "Brandon Sanderson - Mistborn The Final Empire (epub)",
        "guid-cascade-1",
        Some(source_id),
    );
    mock_idx.set_search_results(vec![release]);

    let resp = server
        .admin_client()
        .post_json(
            "/api/v1/want",
            &json!({
                "source_system": "kobbler",
                "source_id": "cascade-early",
                "callback_url": "http://localhost:1234/callback",
                "patterns": [
                    "Mistborn The Final Empire",
                    "Brandon Sanderson Mistborn",
                    "Mistborn epub"
                ],
                "match_targets": [{
                    "id": "target-1",
                    "target": "Brandon Sanderson Mistborn The Final Empire",
                }],
                "media_type": "ebooks",
                "auto_download": true,
                "min_score": 0.5,
                "early_exit_score": 0.7,
            }),
        )
        .await;
    resp.assert_ok();

    let body: Value = resp.json();
    assert_eq!(body["status"], "downloading");
    assert!(body["best_score"].as_f64().unwrap() >= 0.7);
    // selected_release should exist since we found a match
    assert!(
        body["selected_release"].is_object(),
        "should have selected_release: {body}"
    );

    server.cleanup().await;
}

/// When the first pattern yields no results, the cascade continues to the next.
#[tokio::test]
async fn test_search_cascade_fallthrough() {
    let (server, _mock_dl, mock_idx) = start_mock_server().await;
    let pool = &server.pool;

    seed_download_client(pool).await;
    let source_id = seed_indexer_source(pool).await;

    // Mock returns a release. Regardless of which pattern is searched,
    // the mock always returns this release. The scoring against match_targets
    // determines whether it's a good match.
    let release = make_release(
        "Sanderson - Mistborn (epub)",
        "guid-fallthrough",
        Some(source_id),
    );
    mock_idx.set_search_results(vec![release]);

    let resp = server
        .admin_client()
        .post_json(
            "/api/v1/want",
            &json!({
                "source_system": "kobbler",
                "source_id": "cascade-fall",
                "callback_url": "http://localhost:1234/callback",
                "patterns": [
                    "Nonexistent Pattern XYZ",
                    "Sanderson Mistborn",
                ],
                "match_targets": [{
                    "id": "target-fall",
                    "target": "Sanderson Mistborn",
                }],
                "media_type": "ebooks",
                "auto_download": true,
                "min_score": 0.4,
                "early_exit_score": 0.99,
            }),
        )
        .await;
    resp.assert_ok();

    let body: Value = resp.json();
    // Results should be found across the cascade.
    assert!(
        body["results_found"].as_u64().unwrap() > 0,
        "should find results via cascade: {body}"
    );

    server.cleanup().await;
}

// =============================================================================
// Multi-Target Tests
// =============================================================================

/// When multiple match targets are provided with auto_download,
/// the best result per target triggers a download (up to one per target).
#[tokio::test]
async fn test_search_multi_target_auto_download() {
    let (server, _mock_dl, mock_idx) = start_mock_server().await;
    let pool = &server.pool;

    seed_download_client(pool).await;
    let source_id = seed_indexer_source(pool).await;

    // Two releases matching different targets.
    let releases = vec![
        make_release(
            "Brandon Sanderson - Mistborn The Final Empire (epub)",
            "guid-multi-1",
            Some(source_id),
        ),
        make_release(
            "Brandon Sanderson - The Well of Ascension (epub)",
            "guid-multi-2",
            Some(source_id),
        ),
    ];
    mock_idx.set_search_results(releases);

    let resp = server
        .admin_client()
        .post_json(
            "/api/v1/want",
            &json!({
                "source_system": "kobbler",
                "source_id": "multi-target",
                "callback_url": "http://localhost:1234/callback",
                "patterns": ["Mistborn", "Well of Ascension"],
                "match_targets": [
                    {
                        "id": "target-book1",
                        "target": "Brandon Sanderson Mistborn The Final Empire",
                    },
                    {
                        "id": "target-book2",
                        "target": "Brandon Sanderson The Well of Ascension",
                    },
                ],
                "media_type": "ebooks",
                "auto_download": true,
                "min_score": 0.4,
            }),
        )
        .await;
    resp.assert_ok();

    let body: Value = resp.json();
    assert_eq!(body["status"], "downloading", "body: {body}");
    assert!(body["download_id"].is_string());
    assert!(body["results_found"].as_u64().unwrap() >= 2);

    // Verify download record exists in the database.
    let download_id: Uuid = body["download_id"]
        .as_str()
        .unwrap()
        .parse()
        .expect("valid uuid");
    let dl_exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM downloads WHERE id = $1)")
            .bind(download_id)
            .fetch_one(pool)
            .await
            .expect("query download");
    assert!(dl_exists, "download record should exist");

    server.cleanup().await;
}

// =============================================================================
// NATS Handler Search Tests
// =============================================================================

/// Test the WantRequestHandler via NATS payload when indexer returns results.
/// Unlike the basic handler test which has empty results, this one verifies
/// the handler triggers a download when a good match is found.
#[tokio::test]
async fn test_want_handler_search_finds_and_downloads() {
    let (server, _mock_dl, mock_idx) = start_mock_server().await;
    let pool = &server.pool;

    seed_download_client(pool).await;
    let source_id = seed_indexer_source(pool).await;

    let release = make_release(
        "Patrick Rothfuss - The Name of the Wind (epub)",
        "guid-handler-search",
        Some(source_id),
    );
    mock_idx.set_search_results(vec![release]);

    use kobload_api::jobs::handlers::WantRequestHandler;
    use kobload_api::jobs::worker::JobHandler;
    use kobload_api::nats::NatsState;
    use tokio_util::sync::CancellationToken;

    let handler = WantRequestHandler::new(
        server.pool.clone(),
        NatsState::new(None),
        crate::common::local_only_http_client(),
        server.app_state.download_events.clone(),
        server.app_state.download_clients.clone(),
        server.app_state.indexers.clone(),
        server.app_state.xem_cache.clone(),
        server.app_state.scene_mappings.clone(),
        server.app_state.trash_filters.clone(),
        server.app_state.pattern_search_locks.clone(),
    );

    let payload = json!({
        "source_system": "kobbler",
        "source_id": "handler-search-dl",
        "callback_url": "http://localhost:1234/callback",
        "patterns": ["Name of the Wind"],
        "match_targets": [{
            "id": "book-notw",
            "target": "Patrick Rothfuss The Name of the Wind",
        }],
        "match_mode": "fuzzy",
        "min_score": 0.4,
        "early_exit_score": 0.85,
        "media_type": "ebooks",
        "auto_download": true,
        "limit": 100,
    });

    let result = handler
        .execute(Uuid::now_v7(), payload, CancellationToken::new())
        .await;
    assert!(result.is_ok(), "handler should succeed: {:?}", result.err());

    let result_json = result.unwrap();
    assert_eq!(
        result_json["status"], "downloading",
        "should auto-download: {result_json}"
    );
    assert!(result_json["download_id"].is_string());
    assert!(result_json["results_found"].as_u64().unwrap() > 0);
    assert!(result_json["best_score"].as_f64().unwrap() >= 0.4);

    // Verify download record exists
    let download_id: Uuid = result_json["download_id"]
        .as_str()
        .unwrap()
        .parse()
        .expect("valid uuid");
    let dl_exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM downloads WHERE id = $1)")
            .bind(download_id)
            .fetch_one(pool)
            .await
            .expect("query download");
    assert!(dl_exists, "download record should exist");

    server.cleanup().await;
}

/// Test the handler search path when auto_download=false but results exist.
/// Should return results metadata without creating a download.
#[tokio::test]
async fn test_want_handler_search_results_no_download() {
    let (server, _mock_dl, mock_idx) = start_mock_server().await;
    let pool = &server.pool;

    seed_download_client(pool).await;
    let source_id = seed_indexer_source(pool).await;

    let release = make_release(
        "Patrick Rothfuss - The Name of the Wind (epub)",
        "guid-handler-nodl",
        Some(source_id),
    );
    mock_idx.set_search_results(vec![release]);

    use kobload_api::jobs::handlers::WantRequestHandler;
    use kobload_api::jobs::worker::JobHandler;
    use kobload_api::nats::NatsState;
    use tokio_util::sync::CancellationToken;

    let handler = WantRequestHandler::new(
        server.pool.clone(),
        NatsState::new(None),
        crate::common::local_only_http_client(),
        server.app_state.download_events.clone(),
        server.app_state.download_clients.clone(),
        server.app_state.indexers.clone(),
        server.app_state.xem_cache.clone(),
        server.app_state.scene_mappings.clone(),
        server.app_state.trash_filters.clone(),
        server.app_state.pattern_search_locks.clone(),
    );

    let payload = json!({
        "source_system": "kobbler",
        "source_id": "handler-search-nodl",
        "callback_url": "http://localhost:1234/callback",
        "patterns": ["Name of the Wind"],
        "match_targets": [{
            "id": "book-notw-nodl",
            "target": "Patrick Rothfuss The Name of the Wind",
        }],
        "match_mode": "fuzzy",
        "min_score": 0.4,
        "early_exit_score": 0.85,
        "media_type": "ebooks",
        "auto_download": false,
        "limit": 100,
    });

    let result = handler
        .execute(Uuid::now_v7(), payload, CancellationToken::new())
        .await;
    assert!(result.is_ok(), "handler should succeed: {:?}", result.err());

    let result_json = result.unwrap();
    assert_eq!(result_json["status"], "watching");
    assert!(result_json["download_id"].is_null());
    // Results were found even though we didn't download.
    assert!(result_json["results_found"].as_u64().unwrap() > 0);
    assert!(result_json["best_score"].as_f64().unwrap() >= 0.4);

    server.cleanup().await;
}

// =============================================================================
// Search Cache Tests
// =============================================================================

fn tv_episode_want(show_name: &str, season: &str, episode: &str) -> Value {
    json!({
        "kind": "tv_episode",
        "show_name": show_name,
        "season": season,
        "episode": episode,
        "year": 2024,
        "identifiers": null,
        "quality_profile_id": null,
        "quality_overrides": null,
    })
}

/// Metadata-driven searches send an empty `query` and carry everything in
/// `media_want`, so a cache key built from the query alone collides across
/// every episode/movie/book — the second search would replay the first one's
/// results for the whole TTL.
#[tokio::test]
async fn test_search_cache_distinguishes_media_want() {
    let (server, _mock_dl, mock_idx) = start_mock_server().await;
    let pool = &server.pool;

    seed_download_client(pool).await;
    let source_id = seed_indexer_source(pool).await;

    mock_idx.set_search_results(vec![make_release(
        "Cache Show S01E01 1080p",
        "guid-cache-s01e01",
        Some(source_id),
    )]);

    let resp = server
        .admin_client()
        .post_json(
            "/api/v1/search",
            &json!({
                "query": "",
                "media_want": tv_episode_want("Cache Show", "1", "1"),
            }),
        )
        .await;
    resp.assert_ok();
    let body: Value = resp.json();
    assert_eq!(body["results"][0]["guid"], "guid-cache-s01e01");

    mock_idx.set_search_results(vec![make_release(
        "Cache Show S01E02 1080p",
        "guid-cache-s01e02",
        Some(source_id),
    )]);

    let resp = server
        .admin_client()
        .post_json(
            "/api/v1/search",
            &json!({
                "query": "",
                "media_want": tv_episode_want("Cache Show", "1", "2"),
            }),
        )
        .await;
    resp.assert_ok();
    let body: Value = resp.json();
    assert_eq!(
        body["results"][0]["guid"], "guid-cache-s01e02",
        "second episode must not reuse the first episode's cached results: {body}"
    );

    // Same media_want still hits the cache — the entry is scoped, not disabled.
    mock_idx.set_search_results(vec![make_release(
        "Cache Show S01E02 2160p",
        "guid-cache-s01e02-upgrade",
        Some(source_id),
    )]);
    let resp = server
        .admin_client()
        .post_json(
            "/api/v1/search",
            &json!({
                "query": "",
                "media_want": tv_episode_want("Cache Show", "1", "2"),
            }),
        )
        .await;
    resp.assert_ok();
    let body: Value = resp.json();
    assert_eq!(body["results"][0]["guid"], "guid-cache-s01e02");

    server.cleanup().await;
}
