//! E2E tests for the want flow.
//!
//! These tests use mock download clients and indexers injected via the resolver
//! traits, exercising the full HTTP API path from POST /api/v1/want through
//! search, scoring, and download creation.

mod common;

use std::sync::Arc;

use serde_json::{Value, json};
use uuid::Uuid;

use common::{
    MockDownloadClient, MockDownloadClientResolver, MockIndexer, MockIndexerResolver, TestServer,
    TestServerOptions,
};
use kobload_api::jobs::handlers::WantRequestHandler;
use kobload_api::jobs::worker::JobHandler;
use kobload_api::nats::NatsState;
use kobload_api::repositories::{clients as clients_repo, watch as watch_repo};
use kobload_api::watch::Release;
use tokio_util::sync::CancellationToken;

/// Helper: start a test server with mock resolvers, returning the server and mock handles.
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

/// Helper: create a usenet download client in the DB so submit_download can find one.
async fn seed_download_client(pool: &sqlx::PgPool) -> uuid::Uuid {
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

/// Helper: create a newznab indexer source so the search path finds it.
async fn seed_indexer_source(pool: &sqlx::PgPool) -> uuid::Uuid {
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

/// Build a sample Release that will score well against a pattern.
fn make_release(title: &str, guid: &str, source_id: Option<uuid::Uuid>) -> Release {
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

/// Build a torrent magnet release — skips the URL fetch step so the mock
/// download client returns a real "downloading" status instead of "failed".
fn make_magnet_release(title: &str, guid: &str, source_id: Option<uuid::Uuid>) -> Release {
    let mut r = make_release(title, guid, source_id);
    r.protocol = "torrent".to_string();
    r.download_url = None;
    r.magnet_url = Some(format!(
        "magnet:?xt=urn:btih:{:040x}",
        guid.bytes().fold(0u64, |a, b| a.wrapping_add(b as u64))
    ));
    r
}

/// Seed a torrent download client so magnet releases route to the mock.
async fn seed_torrent_download_client(pool: &sqlx::PgPool) -> uuid::Uuid {
    clients_repo::create_client(
        pool,
        clients_repo::DownloadClientInput {
            name: "Mock Torrent Client".to_string(),
            client_type: "mock".to_string(),
            protocol: "torrent".to_string(),
            base_url: "http://localhost:9999".to_string(),
            is_default: true,
            enabled: true,
            ..Default::default()
        },
    )
    .await
    .expect("create torrent download client")
}

// =============================================================================
// Tests
// =============================================================================

#[tokio::test]
async fn test_want_found_auto_downloads() {
    let (server, _mock_dl, mock_idx) = start_mock_server().await;
    let pool = &server.pool;

    seed_download_client(pool).await;
    let source_id = seed_indexer_source(pool).await;

    // Mock indexer returns a release whose title exactly matches the search pattern.
    let release = make_release(
        "Brandon Sanderson - Mistborn The Final Empire (epub)",
        "guid-001",
        Some(source_id),
    );
    mock_idx.set_search_results(vec![release]);

    let resp = server
        .admin_client()
        .post_json(
            "/api/v1/want",
            &json!({
                "source_system": "kobbler",
                "source_id": "book-abc",
                "callback_url": "http://localhost:1234/callback",
                "patterns": ["Mistborn The Final Empire"],
                "match_targets": [{
                    "id": "book-abc",
                    "target": "Brandon Sanderson Mistborn The Final Empire",
                }],
                "media_type": "ebooks",
                "auto_download": true,
                "min_score": 0.5,
            }),
        )
        .await;
    resp.assert_ok();

    let body: Value = resp.json();
    assert_eq!(body["status"], "downloading", "body: {body}");
    assert!(body["download_id"].is_string(), "should have download_id");
    assert!(body["watch_pattern_id"].is_string());
    assert!(body["results_found"].as_u64().unwrap() > 0);
    assert!(body["best_score"].as_f64().unwrap() >= 0.5);

    // Verify watch pattern was created in DB
    let pattern = watch_repo::get_pattern_by_source(pool, "kobbler", "book-abc")
        .await
        .expect("query")
        .expect("pattern should exist");
    assert!(pattern.auto_download);

    // Verify download record exists
    let download_id: uuid::Uuid = body["download_id"]
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
    assert!(dl_exists, "download record should exist in DB");

    server.cleanup().await;
}

#[tokio::test]
async fn test_want_not_found_creates_watch() {
    let (server, _mock_dl, mock_idx) = start_mock_server().await;
    let pool = &server.pool;

    seed_download_client(pool).await;
    seed_indexer_source(pool).await;

    // Mock indexer returns empty — no matches.
    mock_idx.set_search_results(vec![]);

    let resp = server
        .admin_client()
        .post_json(
            "/api/v1/want",
            &json!({
                "source_system": "kobbler",
                "source_id": "book-nope",
                "callback_url": "http://localhost:1234/callback",
                "patterns": ["Nonexistent Book Title XYZ"],
                "media_type": "ebooks",
                "auto_download": true,
            }),
        )
        .await;
    resp.assert_ok();

    let body: Value = resp.json();
    assert_eq!(body["status"], "watching");
    assert!(body["download_id"].is_null());
    assert_eq!(body["results_found"], 0);

    // Watch pattern should still exist for future RSS matching.
    let pattern = watch_repo::get_pattern_by_source(pool, "kobbler", "book-nope")
        .await
        .expect("query")
        .expect("pattern should exist");
    assert!(pattern.enabled);

    server.cleanup().await;
}

#[tokio::test]
async fn test_want_confirm_deletes_pattern() {
    let (server, _mock_dl, mock_idx) = start_mock_server().await;
    let pool = &server.pool;

    seed_download_client(pool).await;
    seed_indexer_source(pool).await;

    mock_idx.set_search_results(vec![]);

    // Create a want (which creates a watch pattern).
    let resp = server
        .admin_client()
        .post_json(
            "/api/v1/want",
            &json!({
                "source_system": "kobbler",
                "source_id": "book-confirm",
                "callback_url": "http://localhost:1234/callback",
                "patterns": ["Some Book"],
                "media_type": "ebooks",
                "upgrade_allowed": false,
            }),
        )
        .await;
    resp.assert_ok();

    // Verify pattern exists.
    assert!(
        watch_repo::pattern_exists(pool, "kobbler", "book-confirm")
            .await
            .expect("query")
    );

    // Confirm the want — should delete the pattern.
    let resp = server
        .admin_client()
        .post("/api/v1/want/kobbler/book-confirm/confirm")
        .await;
    resp.assert_status(reqwest::StatusCode::NO_CONTENT);

    // Pattern should be gone.
    assert!(
        !watch_repo::pattern_exists(pool, "kobbler", "book-confirm")
            .await
            .expect("query")
    );

    server.cleanup().await;
}

#[tokio::test]
async fn test_want_scoring_below_threshold() {
    let (server, _mock_dl, mock_idx) = start_mock_server().await;
    let pool = &server.pool;

    seed_download_client(pool).await;
    let source_id = seed_indexer_source(pool).await;

    // Release title is very different from the pattern, so score will be low.
    let release = make_release(
        "Completely Unrelated Title About Cooking",
        "guid-low",
        Some(source_id),
    );
    mock_idx.set_search_results(vec![release]);

    let resp = server
        .admin_client()
        .post_json(
            "/api/v1/want",
            &json!({
                "source_system": "kobbler",
                "source_id": "book-low",
                "callback_url": "http://localhost:1234/callback",
                "patterns": ["Brandon Sanderson Mistborn"],
                "match_targets": [{
                    "id": "book-low",
                    "target": "Brandon Sanderson Mistborn The Final Empire",
                }],
                "media_type": "ebooks",
                "min_score": 0.95,
                "auto_download": true,
            }),
        )
        .await;
    resp.assert_ok();

    let body: Value = resp.json();
    assert_eq!(
        body["status"], "watching",
        "low-scoring result shouldn't trigger download"
    );
    assert!(body["download_id"].is_null());

    server.cleanup().await;
}

#[tokio::test]
async fn test_want_auto_download_disabled() {
    let (server, _mock_dl, mock_idx) = start_mock_server().await;
    let pool = &server.pool;

    seed_download_client(pool).await;
    let source_id = seed_indexer_source(pool).await;

    // Good match, but auto_download is false.
    let release = make_release(
        "Brandon Sanderson - Mistborn The Final Empire (epub)",
        "guid-noauto",
        Some(source_id),
    );
    mock_idx.set_search_results(vec![release]);

    let resp = server
        .admin_client()
        .post_json(
            "/api/v1/want",
            &json!({
                "source_system": "kobbler",
                "source_id": "book-noauto",
                "callback_url": "http://localhost:1234/callback",
                "patterns": ["Mistborn The Final Empire"],
                "match_targets": [{
                    "id": "book-noauto",
                    "target": "Brandon Sanderson Mistborn The Final Empire",
                }],
                "media_type": "ebooks",
                "auto_download": false,
                "min_score": 0.5,
            }),
        )
        .await;
    resp.assert_ok();

    let body: Value = resp.json();
    assert_eq!(
        body["status"], "watching",
        "auto_download=false should not download"
    );
    assert!(body["download_id"].is_null());
    // Results were found even though we didn't download.
    assert!(body["results_found"].as_u64().unwrap() > 0);

    server.cleanup().await;
}

#[tokio::test]
async fn test_want_requires_auth() {
    let (server, _, _) = start_mock_server().await;

    let resp = server
        .anon_client()
        .post_json(
            "/api/v1/want",
            &json!({
                "source_system": "test",
                "source_id": "1",
                "callback_url": "http://localhost/cb",
                "patterns": ["test"],
            "media_type": "ebooks",
            }),
        )
        .await;
    resp.assert_status(reqwest::StatusCode::UNAUTHORIZED);

    server.cleanup().await;
}

#[tokio::test]
async fn test_want_requires_patterns_or_query() {
    let (server, _, _) = start_mock_server().await;

    // No patterns or query -> should fail.
    let resp = server
        .admin_client()
        .post_json(
            "/api/v1/want",
            &json!({
                "source_system": "kobbler",
                "source_id": "book-1",
                "callback_url": "http://localhost/cb",
            }),
        )
        .await;
    resp.assert_status(reqwest::StatusCode::BAD_REQUEST);

    server.cleanup().await;
}

#[tokio::test]
async fn test_want_updates_existing_pattern() {
    let (server, _mock_dl, mock_idx) = start_mock_server().await;
    let pool = &server.pool;

    seed_download_client(pool).await;
    seed_indexer_source(pool).await;

    mock_idx.set_search_results(vec![]);

    // First want creates the pattern.
    let resp = server
        .admin_client()
        .post_json(
            "/api/v1/want",
            &json!({
                "source_system": "kobbler",
                "source_id": "book-update",
                "callback_url": "http://localhost:1234/callback",
                "patterns": ["Old Pattern"],
                "media_type": "ebooks",
            }),
        )
        .await;
    resp.assert_ok();
    let first_id: String =
        serde_json::from_value(resp.json::<Value>()["watch_pattern_id"].clone()).unwrap();

    // Second want with same source_system/source_id should update, not duplicate.
    let resp = server
        .admin_client()
        .post_json(
            "/api/v1/want",
            &json!({
                "source_system": "kobbler",
                "source_id": "book-update",
                "callback_url": "http://localhost:5555/new-callback",
                "patterns": ["New Pattern"],
                "media_type": "ebooks",
            }),
        )
        .await;
    resp.assert_ok();
    let second_id: String =
        serde_json::from_value(resp.json::<Value>()["watch_pattern_id"].clone()).unwrap();

    // Should be the same pattern, updated in place.
    assert_eq!(first_id, second_id);

    let pattern = watch_repo::get_pattern_by_source(pool, "kobbler", "book-update")
        .await
        .expect("query")
        .expect("pattern");
    assert_eq!(pattern.callback_url, "http://localhost:5555/new-callback");
    assert_eq!(pattern.patterns, vec!["New Pattern"]);

    server.cleanup().await;
}

/// Regression: clicking "want" on a second episode of the same season must not
/// be silently swallowed by the pattern-level fast-path. Both episodes share
/// one TvSeason watch_pattern (kobbler keys it on season_id), but each episode
/// needs its own search; bailing because the pattern "has an active download"
/// strands every episode after the first.
#[tokio::test]
async fn test_want_tv_season_second_episode_triggers_search() {
    let (server, _mock_dl, mock_idx) = start_mock_server().await;
    let pool = &server.pool;

    seed_torrent_download_client(pool).await;
    let source_id = seed_indexer_source(pool).await;

    // First want: only E1 is needed. Mock returns a magnet release that parses as S01E01.
    // Magnet URLs skip fetch_release_payload so the download lands as 'downloading',
    // not 'failed' — without that, pattern_has_active_download would be false and the
    // fast-path the test is targeting never runs.
    let release_e1 = make_magnet_release(
        "Demo Show S01E01 1080p WEB-DL x264",
        "guid-e1",
        Some(source_id),
    );
    mock_idx.set_search_results(vec![release_e1]);

    let resp = server
        .admin_client()
        .post_json(
            "/api/v1/want",
            &json!({
                "source_system": "kobbler",
                "source_id": "season-bug",
                "callback_url": "http://localhost:1234/callback",
                "patterns": ["Demo Show"],
                "media_type": "tv",
                "auto_download": true,
                "min_score": 0.5,
                // Simulate back-catalog episodes (kobbler sets this to false
                // when no episode aired in the last 30 days). With
                // upgrade_allowed=true the fast-path is bypassed anyway, so
                // the bug only manifests on older shows.
                "upgrade_allowed": false,
                "media_want": {
                    "kind": "tv_season",
                    "show_name": "Demo Show",
                    "season": "1",
                    "episodes_needed": [
                        {"season": "1", "episode": "1", "sort_order": 1},
                    ],
                    "pack_preference": "auto",
                },
            }),
        )
        .await;
    resp.assert_ok();
    let body: Value = resp.json();
    assert_eq!(
        body["status"], "downloading",
        "first want must download E1: {body}"
    );
    let first_download = body["download_id"]
        .as_str()
        .expect("E1 download_id in first response")
        .to_string();

    // Sanity: the pattern now has an active download — the precondition the
    // pre-fix fast-path tripped on.
    let pattern_row = watch_repo::get_pattern_by_source(pool, "kobbler", "season-bug")
        .await
        .expect("query")
        .expect("pattern");
    assert!(
        watch_repo::pattern_has_active_download(pool, pattern_row.id)
            .await
            .expect("query"),
        "first want should leave an active download on the pattern"
    );

    // Second want: same season pattern (same source_id), now also wanting E2.
    // Mock now returns an S01E02 release.
    let release_e2 = make_magnet_release(
        "Demo Show S01E02 1080p WEB-DL x264",
        "guid-e2",
        Some(source_id),
    );
    mock_idx.set_search_results(vec![release_e2]);

    let resp = server
        .admin_client()
        .post_json(
            "/api/v1/want",
            &json!({
                "source_system": "kobbler",
                "source_id": "season-bug",
                "callback_url": "http://localhost:1234/callback",
                "patterns": ["Demo Show"],
                "media_type": "tv",
                "auto_download": true,
                "min_score": 0.5,
                // Simulate back-catalog episodes (kobbler sets this to false
                // when no episode aired in the last 30 days). With
                // upgrade_allowed=true the fast-path is bypassed anyway, so
                // the bug only manifests on older shows.
                "upgrade_allowed": false,
                "media_want": {
                    "kind": "tv_season",
                    "show_name": "Demo Show",
                    "season": "1",
                    "episodes_needed": [
                        {"season": "1", "episode": "1", "sort_order": 1},
                        {"season": "1", "episode": "2", "sort_order": 2},
                    ],
                    "pack_preference": "auto",
                },
            }),
        )
        .await;
    resp.assert_ok();
    let body: Value = resp.json();
    assert_eq!(
        body["status"], "downloading",
        "second want must run a search & download E2, not silently fast-path: {body}"
    );
    assert!(
        body["results_found"].as_u64().unwrap_or(0) > 0,
        "second want must have actually searched: {body}"
    );
    let second_download = body["download_id"]
        .as_str()
        .expect("E2 download_id in second response")
        .to_string();
    assert_ne!(
        first_download, second_download,
        "second want must produce a distinct download for E2 (got the same id as E1)"
    );

    // Confirm both downloads survive in the DB.
    let dl_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM downloads")
        .fetch_one(pool)
        .await
        .expect("count downloads");
    assert_eq!(dl_count, 2, "expected one download per episode");

    server.cleanup().await;
}

#[tokio::test]
async fn test_bulk_want() {
    let (server, _mock_dl, mock_idx) = start_mock_server().await;
    let pool = &server.pool;

    seed_download_client(pool).await;
    seed_indexer_source(pool).await;

    mock_idx.set_search_results(vec![]);

    let resp = server
        .admin_client()
        .post_json(
            "/api/v1/want/bulk",
            &json!([
                {
                    "source_system": "kobbler",
                    "source_id": "bulk-1",
                    "callback_url": "http://localhost/cb",
                    "patterns": ["Book One"],
                    "media_type": "ebooks",
                },
                {
                    "source_system": "kobbler",
                    "source_id": "bulk-2",
                    "callback_url": "http://localhost/cb",
                    "patterns": ["Book Two"],
                    "media_type": "ebooks",
                },
                {
                    // This one is missing patterns and query -> should fail individually.
                    "source_system": "kobbler",
                    "source_id": "bulk-3",
                    "callback_url": "http://localhost/cb",
                },
            ]),
        )
        .await;
    resp.assert_ok();

    let body: Value = resp.json();
    assert_eq!(body["succeeded"], 2);
    assert_eq!(body["failed"], 1);
    assert_eq!(body["results"].as_array().unwrap().len(), 3);

    server.cleanup().await;
}

// =============================================================================
// WantRequestHandler direct tests (Area 3b)
// =============================================================================

#[tokio::test]
async fn test_want_handler_processes_nats_payload() {
    let (server, _mock_dl, mock_idx) = start_mock_server().await;
    let pool = &server.pool;

    seed_download_client(pool).await;
    seed_indexer_source(pool).await;

    mock_idx.set_search_results(vec![]);

    let handler = WantRequestHandler::new(
        server.pool.clone(),
        NatsState::new(None),
        reqwest::Client::new(),
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
        "source_id": "handler-test-1",
        "callback_url": "http://localhost:1234/callback",
        "patterns": ["Mistborn The Final Empire"],
        "match_targets": [],
        "match_mode": "fuzzy",
        "min_score": 0.5,
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
    assert!(
        result_json["watch_pattern_id"].is_string(),
        "should have watch_pattern_id"
    );
    // No results found (empty search), so status should be "watching"
    assert_eq!(result_json["status"], "watching");
    assert_eq!(result_json["results_found"], 0);

    server.cleanup().await;
}

#[tokio::test]
async fn test_want_handler_rejects_invalid_payload() {
    let (server, _mock_dl, _mock_idx) = start_mock_server().await;

    let handler = WantRequestHandler::new(
        server.pool.clone(),
        NatsState::new(None),
        reqwest::Client::new(),
        server.app_state.download_events.clone(),
        server.app_state.download_clients.clone(),
        server.app_state.indexers.clone(),
        server.app_state.xem_cache.clone(),
        server.app_state.scene_mappings.clone(),
        server.app_state.trash_filters.clone(),
        server.app_state.pattern_search_locks.clone(),
    );

    // Invalid payload - missing required fields
    let payload = json!({ "not_a_real_field": "value" });

    let result = handler
        .execute(Uuid::now_v7(), payload, CancellationToken::new())
        .await;
    assert!(result.is_err(), "handler should fail with invalid payload");

    server.cleanup().await;
}

/// "Get me another copy" from chat is a want, not a release pick: Kobbler hands
/// the choice back to Kobload and only asserts that a person asked for it now.
/// Without `manual` the episode is already imported and the want silently
/// searches nothing, which is a regrab that does nothing at all.
#[tokio::test]
async fn test_manual_want_reopens_an_already_imported_episode() {
    use kobload_api::repositories::downloads as downloads_repo;

    let (server, _mock_dl, mock_idx) = start_mock_server().await;
    let pool = &server.pool;

    seed_torrent_download_client(pool).await;
    let source_id = seed_indexer_source(pool).await;

    let want_body = |manual: bool, guid_note: &str| {
        json!({
            "source_system": "kobbler",
            "source_id": "regrab-season",
            "callback_url": "http://localhost:1234/callback",
            "patterns": ["Demo Show"],
            "media_type": "tv",
            "auto_download": true,
            "min_score": 0.5,
            "upgrade_allowed": false,
            "manual": manual,
            "metadata": {"note": guid_note},
            "media_want": {
                "kind": "tv_season",
                "show_name": "Demo Show",
                "season": "1",
                "episodes_needed": [
                    {"season": "1", "episode": "1", "sort_order": 1},
                ],
                "pack_preference": "auto",
            },
        })
    };

    mock_idx.set_search_results(vec![make_magnet_release(
        "Demo Show S01E01 1080p WEB-DL x264-FIRST",
        "guid-first",
        Some(source_id),
    )]);
    let resp = server
        .admin_client()
        .post_json("/api/v1/want", &want_body(false, "first"))
        .await;
    resp.assert_ok();
    let body: Value = resp.json();
    assert_eq!(
        body["status"], "downloading",
        "first want must grab: {body}"
    );
    let first_download = Uuid::parse_str(body["download_id"].as_str().expect("download_id"))
        .expect("download id is a uuid");

    // The copy lands and is imported — the state that makes every later want a
    // no-op unless somebody explicitly asks again.
    downloads_repo::confirm_import(pool, first_download)
        .await
        .expect("confirm import");
    watch_repo::mark_match_import_confirmed(pool, first_download)
        .await
        .expect("mark match imported");

    mock_idx.set_search_results(vec![make_magnet_release(
        "Demo Show S01E01 1080p WEB-DL x264-SECOND",
        "guid-second",
        Some(source_id),
    )]);

    let resp = server
        .admin_client()
        .post_json("/api/v1/want", &want_body(false, "auto retry"))
        .await;
    resp.assert_ok();
    let body: Value = resp.json();
    assert!(
        body["download_id"].is_null(),
        "an ordinary want must leave an imported episode alone: {body}"
    );

    let resp = server
        .admin_client()
        .post_json("/api/v1/want", &want_body(true, "chat regrab"))
        .await;
    resp.assert_ok();
    let body: Value = resp.json();
    let second_download = body["download_id"]
        .as_str()
        .unwrap_or_else(|| panic!("a manual want must grab another copy: {body}"));
    assert_ne!(
        second_download,
        first_download.to_string(),
        "the regrab has to be a different download"
    );

    server.cleanup().await;
}

/// Kobload blocklists a release itself when its download fails
/// (`trigger_research_for_download`). Kobbler only ever sends the exclusions it
/// knows about, so a sync that overwrote the column handed the very next search
/// back the release it had just watched fail.
#[tokio::test]
async fn test_want_sync_merges_exclusions_instead_of_overwriting_kobloads_own() {
    let (server, _mock_dl, _mock_idx) = start_mock_server().await;
    let pool = &server.pool;

    seed_torrent_download_client(pool).await;
    seed_indexer_source(pool).await;

    let want = |excluded: Value| {
        json!({
            "source_system": "kobbler",
            "source_id": "exclusion-merge",
            "callback_url": "http://localhost:1234/callback",
            "patterns": ["Demo Show"],
            "media_type": "tv",
            "auto_download": false,
            "min_score": 0.5,
            "excluded_releases": excluded,
        })
    };

    server
        .admin_client()
        .post_json(
            "/api/v1/want",
            &want(json!([{"release_title": "kobbler-rejected"}])),
        )
        .await
        .assert_ok();

    let pattern = watch_repo::get_pattern_by_source(pool, "kobbler", "exclusion-merge")
        .await
        .expect("query")
        .expect("pattern");

    // Kobload's own: a download for this pattern failed and was blocklisted.
    watch_repo::append_excluded_release(
        pool,
        pattern.id,
        &json!([{"release_title": "failed-to-download"}]),
    )
    .await
    .expect("append exclusion");

    // Kobbler re-wants, sending only what it knows about.
    server
        .admin_client()
        .post_json(
            "/api/v1/want",
            &want(json!([{"release_title": "kobbler-rejected"}])),
        )
        .await
        .assert_ok();

    let pattern = watch_repo::get_pattern_by_source(pool, "kobbler", "exclusion-merge")
        .await
        .expect("query")
        .expect("pattern");
    let excluded = pattern
        .excluded_releases
        .expect("excluded_releases survives a sync");
    let titles: Vec<&str> = excluded
        .as_array()
        .expect("array")
        .iter()
        .filter_map(|e| e["release_title"].as_str())
        .collect();

    assert!(
        titles.contains(&"failed-to-download"),
        "the sync threw away Kobload's own exclusion: {excluded}"
    );
    assert!(titles.contains(&"kobbler-rejected"), "{excluded}");
    assert_eq!(
        titles.len(),
        2,
        "re-sending the same blocklist must not accumulate duplicates: {excluded}"
    );

    server.cleanup().await;
}

/// The exclusions are merged into the pattern, but the want's own search used
/// to filter only by what the request carried. A release Kobload had already
/// watched fail (a fake "South Park S29E03 ... -FLUX" full of .exe files) was
/// grabbed again by the next kobbler want, which didn't know about it.
#[tokio::test]
async fn test_want_search_skips_releases_kobload_excluded() {
    let (server, _mock_dl, mock_idx) = start_mock_server().await;
    let pool = &server.pool;

    seed_download_client(pool).await;
    let source_id = seed_indexer_source(pool).await;

    let want = |auto_download: bool| {
        json!({
            "source_system": "kobbler",
            "source_id": "book-excluded",
            "callback_url": "http://localhost:1234/callback",
            "patterns": ["Mistborn The Final Empire"],
            "match_targets": [{
                "id": "book-excluded",
                "target": "Brandon Sanderson Mistborn The Final Empire",
            }],
            "media_type": "ebooks",
            "auto_download": auto_download,
            "min_score": 0.5,
            "excluded_releases": [{"release_title": "kobbler-rejected"}],
        })
    };

    server
        .admin_client()
        .post_json("/api/v1/want", &want(false))
        .await
        .assert_ok();
    let pattern = watch_repo::get_pattern_by_source(pool, "kobbler", "book-excluded")
        .await
        .expect("query")
        .expect("pattern");

    // A download of this release failed, so Kobload blocklisted it.
    let failed = "Brandon Sanderson - Mistborn The Final Empire (epub)";
    watch_repo::append_excluded_release(pool, pattern.id, &json!([{ "release_title": failed }]))
        .await
        .expect("append exclusion");

    // Kobbler wants it again; the indexer still serves the failed release.
    mock_idx.set_search_results(vec![make_release(failed, "guid-failed", Some(source_id))]);
    let resp = server
        .admin_client()
        .post_json("/api/v1/want", &want(true))
        .await;
    resp.assert_ok();
    let body: Value = resp.json();
    assert_ne!(
        body["status"], "downloading",
        "re-grabbed an excluded release: {body}"
    );

    let grabbed: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM downloads WHERE name = $1")
        .bind(failed)
        .fetch_one(pool)
        .await
        .expect("count downloads");
    assert_eq!(grabbed, 0);

    server.cleanup().await;
}
