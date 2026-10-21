//! E2E tests for RSS watch pattern matching via `process_release()`.
//!
//! These tests call `process_release` directly with an AppState containing
//! mock resolvers, verifying match creation, dedup, auto-download, and callbacks.

use crate::common;

use std::sync::Arc;

use serde_json::json;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use common::{
    MockDownloadClient, MockDownloadClientResolver, MockIndexer, MockIndexerResolver, TestServer,
    TestServerOptions,
};
use kobload_api::repositories::{clients as clients_repo, watch as watch_repo};
use kobload_api::watch::{Release, process_release};

/// Start a test server with mock resolvers.
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

/// Seed a download client in the DB.
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

/// Seed an indexer source in the DB. Returns the source ID.
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

/// Get the ebooks media type ID.
async fn get_ebooks_media_type_id(pool: &sqlx::PgPool) -> Uuid {
    let row: (Uuid,) =
        sqlx::query_as("SELECT id FROM media_type_config WHERE key = 'ebooks' LIMIT 1")
            .fetch_one(pool)
            .await
            .expect("ebooks media type must exist");
    row.0
}

/// Build a sample Release.
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

/// Confirm the import for a download created by `process_release`.
///
/// These fixtures resolve a `download_url`, which no test server serves, so the
/// download lands `failed`. A failed download is not a real import and
/// `mark_match_import_confirmed` refuses to stamp one, so move it to the state a
/// genuinely-landed download would be in first.
async fn confirm_import(pool: &sqlx::PgPool, download_id: Uuid) {
    sqlx::query(
        "UPDATE downloads SET status = 'completed', progress = 1.0, error = NULL WHERE id = $1",
    )
    .bind(download_id)
    .execute(pool)
    .await
    .expect("land download");

    watch_repo::mark_match_import_confirmed(pool, download_id)
        .await
        .expect("confirm import");

    let stamped: bool = sqlx::query_scalar(
        "SELECT import_confirmed_at IS NOT NULL FROM watch_matches WHERE download_id = $1",
    )
    .bind(download_id)
    .fetch_one(pool)
    .await
    .expect("read import stamp");
    assert!(stamped, "fixture must actually record the import");
}

/// Create a watch pattern in the DB and return (pattern_id, pattern_row).
async fn create_test_pattern(
    pool: &sqlx::PgPool,
    media_type_id: Uuid,
    callback_url: &str,
    patterns: Vec<String>,
    min_score: f64,
    auto_download: bool,
) -> (Uuid, watch_repo::WatchPatternRow) {
    let source_id = Uuid::now_v7().to_string();
    let pattern_id = watch_repo::create_pattern(
        pool,
        "test",
        &source_id,
        watch_repo::WatchPatternInput {
            callback_url: callback_url.to_string(),
            patterns,
            media_want: None,
            match_targets: Some(json!([
                {
                    "id": "target-1",
                    "target": "Brandon Sanderson Mistborn The Final Empire",
                }
            ])),
            match_mode: Some("fuzzy".to_string()),
            min_score: Some(min_score),
            media_type_id: Some(media_type_id),
            auto_download,
            metadata: None,
            enabled: true,
            upgrade_allowed: true,
            search_not_before: None,
        },
    )
    .await
    .expect("create pattern");

    let row = watch_repo::get_pattern_by_id(pool, pattern_id)
        .await
        .expect("query")
        .expect("pattern row");
    (pattern_id, row)
}

// =============================================================================
// Tests
// =============================================================================

#[tokio::test]
async fn test_rss_match_triggers_download() {
    let (server, _mock_dl, _mock_idx) = start_mock_server().await;
    let pool = &server.pool;

    seed_download_client(pool).await;
    let source_id = seed_indexer_source(pool).await;
    let media_type_id = get_ebooks_media_type_id(pool).await;

    let (pattern_id, pattern) = create_test_pattern(
        pool,
        media_type_id,
        "http://localhost:1111/callback", // non-existent, callback will fail but match still recorded
        vec!["Mistborn The Final Empire".to_string()],
        0.5,
        true, // auto_download
    )
    .await;

    let source = watch_repo::get_indexer_source_by_id(pool, source_id)
        .await
        .expect("query")
        .expect("source row");

    let release = make_release(
        "Brandon Sanderson - Mistborn The Final Empire (epub)",
        "guid-rss-001",
        Some(source_id),
    );

    process_release(&server.app_state, &source, &release, &[pattern]).await;

    // Verify match was recorded
    let matches = watch_repo::list_matches_by_pattern(pool, pattern_id, 10)
        .await
        .expect("list matches");
    assert_eq!(matches.len(), 1, "should have exactly 1 match");
    assert_eq!(matches[0].release_guid, "guid-rss-001");
    assert_eq!(matches[0].action, "downloaded");
    assert!(
        matches[0].download_id.is_some(),
        "auto_download=true should create a download"
    );
    assert!(matches[0].match_score >= 0.5);

    server.cleanup().await;
}

#[tokio::test]
async fn test_rss_match_below_threshold() {
    let (server, _mock_dl, _mock_idx) = start_mock_server().await;
    let pool = &server.pool;

    let source_id = seed_indexer_source(pool).await;
    let media_type_id = get_ebooks_media_type_id(pool).await;

    // Very high min_score that won't match
    let (pattern_id, pattern) = create_test_pattern(
        pool,
        media_type_id,
        "http://localhost:1111/callback",
        vec!["Mistborn The Final Empire".to_string()],
        0.99, // extremely high threshold
        true,
    )
    .await;

    let source = watch_repo::get_indexer_source_by_id(pool, source_id)
        .await
        .expect("query")
        .expect("source row");

    // Release title is very different from the target
    let release = make_release(
        "Completely Unrelated Title About Cooking Recipes",
        "guid-rss-low",
        Some(source_id),
    );

    process_release(&server.app_state, &source, &release, &[pattern]).await;

    // No matches should be recorded
    let matches = watch_repo::list_matches_by_pattern(pool, pattern_id, 10)
        .await
        .expect("list matches");
    assert!(
        matches.is_empty(),
        "below-threshold release should not match"
    );

    server.cleanup().await;
}

#[tokio::test]
async fn test_rss_match_dedup() {
    let (server, _mock_dl, _mock_idx) = start_mock_server().await;
    let pool = &server.pool;

    seed_download_client(pool).await;
    let source_id = seed_indexer_source(pool).await;
    let media_type_id = get_ebooks_media_type_id(pool).await;

    let (pattern_id, pattern) = create_test_pattern(
        pool,
        media_type_id,
        "http://localhost:1111/callback",
        vec!["Mistborn The Final Empire".to_string()],
        0.5,
        true,
    )
    .await;

    let source = watch_repo::get_indexer_source_by_id(pool, source_id)
        .await
        .expect("query")
        .expect("source row");

    let release = make_release(
        "Brandon Sanderson - Mistborn The Final Empire (epub)",
        "guid-rss-dedup",
        Some(source_id),
    );

    // Process the same release twice
    process_release(
        &server.app_state,
        &source,
        &release,
        std::slice::from_ref(&pattern),
    )
    .await;
    process_release(
        &server.app_state,
        &source,
        &release,
        std::slice::from_ref(&pattern),
    )
    .await;

    // Should only have 1 match (deduped by already_matched check)
    let matches = watch_repo::list_matches_by_pattern(pool, pattern_id, 10)
        .await
        .expect("list matches");
    assert_eq!(matches.len(), 1, "duplicate release should be deduped");

    server.cleanup().await;
}

/// Watch match callbacks are sent exclusively via NATS. When NATS is unavailable
/// (as in tests), the callback is marked as sent to prevent infinite retries.
#[tokio::test]
async fn test_rss_match_sends_callback() {
    let (server, _mock_dl, _mock_idx) = start_mock_server().await;
    let pool = &server.pool;

    seed_download_client(pool).await;
    let source_id = seed_indexer_source(pool).await;
    let media_type_id = get_ebooks_media_type_id(pool).await;

    let (pattern_id, pattern) = create_test_pattern(
        pool,
        media_type_id,
        "http://localhost:1111/unused-callback",
        vec!["Mistborn The Final Empire".to_string()],
        0.5,
        true,
    )
    .await;

    let source = watch_repo::get_indexer_source_by_id(pool, source_id)
        .await
        .expect("query")
        .expect("source row");

    let release = make_release(
        "Brandon Sanderson - Mistborn The Final Empire (epub)",
        "guid-rss-cb",
        Some(source_id),
    );

    process_release(&server.app_state, &source, &release, &[pattern]).await;

    // Verify match was recorded with callback_sent_at set
    // (NATS unavailable in tests, so callback is marked as sent to prevent retries)
    let matches = watch_repo::list_matches_by_pattern(pool, pattern_id, 10)
        .await
        .expect("list matches");
    assert_eq!(matches.len(), 1);
    assert!(
        matches[0].callback_sent_at.is_some(),
        "callback_sent_at should be set even when NATS is unavailable"
    );

    server.cleanup().await;
}

#[tokio::test]
async fn test_rss_match_no_auto_download() {
    let (server, _mock_dl, _mock_idx) = start_mock_server().await;
    let pool = &server.pool;

    let source_id = seed_indexer_source(pool).await;
    let media_type_id = get_ebooks_media_type_id(pool).await;

    let (pattern_id, pattern) = create_test_pattern(
        pool,
        media_type_id,
        "http://localhost:1111/callback",
        vec!["Mistborn The Final Empire".to_string()],
        0.5,
        false, // auto_download disabled
    )
    .await;

    let source = watch_repo::get_indexer_source_by_id(pool, source_id)
        .await
        .expect("query")
        .expect("source row");

    let release = make_release(
        "Brandon Sanderson - Mistborn The Final Empire (epub)",
        "guid-rss-noauto",
        Some(source_id),
    );

    process_release(&server.app_state, &source, &release, &[pattern]).await;

    let matches = watch_repo::list_matches_by_pattern(pool, pattern_id, 10)
        .await
        .expect("list matches");
    assert_eq!(matches.len(), 1);
    assert_eq!(matches[0].action, "notified");
    assert!(
        matches[0].download_id.is_none(),
        "auto_download=false should not create download"
    );

    server.cleanup().await;
}

// =============================================================================
// Reject → upgrade_until tests
// =============================================================================

/// When the only confirmed-import match for an upgrading pattern is rejected,
/// `upgrade_until` must be cleared so the pattern returns to "searching".
#[tokio::test]
async fn test_reject_match_clears_upgrade_until() {
    let (server, _mock_dl, _mock_idx) = start_mock_server().await;
    let pool = &server.pool;

    seed_download_client(pool).await;
    let source_id = seed_indexer_source(pool).await;
    let media_type_id = get_ebooks_media_type_id(pool).await;

    let (pattern_id, pattern) = create_test_pattern(
        pool,
        media_type_id,
        "http://localhost:1111/callback",
        vec!["Mistborn The Final Empire".to_string()],
        0.5,
        true,
    )
    .await;

    let source = watch_repo::get_indexer_source_by_id(pool, source_id)
        .await
        .expect("query")
        .expect("source row");

    // Create a match via process_release (auto_download = true → downloads it)
    let release = make_release(
        "Brandon Sanderson - Mistborn The Final Empire (epub)",
        "guid-reject-upgrade",
        Some(source_id),
    );
    process_release(&server.app_state, &source, &release, &[pattern]).await;

    let matches = watch_repo::list_matches_by_pattern(pool, pattern_id, 10)
        .await
        .expect("list matches");
    assert_eq!(matches.len(), 1);
    let match_id = matches[0].id;
    let download_id = matches[0].download_id.expect("should have download");

    // Simulate consumer confirming import
    confirm_import(pool, download_id).await;

    // Set upgrade_until (mimics what confirm_want does)
    let until = OffsetDateTime::now_utc() + Duration::hours(720);
    watch_repo::set_upgrade_until(pool, pattern_id, until)
        .await
        .expect("set upgrade_until");

    // Verify the pattern is now in upgrading mode
    let pat = watch_repo::get_pattern_by_id(pool, pattern_id)
        .await
        .expect("query")
        .expect("pattern");
    assert!(pat.upgrade_until.is_some(), "should be upgrading");

    // Reject the match
    watch_repo::reject_match(pool, match_id, Some("wrong content"))
        .await
        .expect("reject match");

    // upgrade_until should be cleared because no non-rejected confirmed imports remain
    let pat = watch_repo::get_pattern_by_id(pool, pattern_id)
        .await
        .expect("query")
        .expect("pattern");
    assert!(
        pat.upgrade_until.is_none(),
        "upgrade_until should be cleared after rejecting the only confirmed import"
    );

    server.cleanup().await;
}

/// When a pattern has multiple confirmed imports and only one is rejected,
/// `upgrade_until` should NOT be cleared (other imports still valid).
#[tokio::test]
async fn test_reject_match_keeps_upgrade_until_with_other_imports() {
    let (server, _mock_dl, _mock_idx) = start_mock_server().await;
    let pool = &server.pool;

    seed_download_client(pool).await;
    let source_id = seed_indexer_source(pool).await;
    let media_type_id = get_ebooks_media_type_id(pool).await;

    let (pattern_id, pattern) = create_test_pattern(
        pool,
        media_type_id,
        "http://localhost:1111/callback",
        vec!["Mistborn The Final Empire".to_string()],
        0.5,
        true,
    )
    .await;

    let source = watch_repo::get_indexer_source_by_id(pool, source_id)
        .await
        .expect("query")
        .expect("source row");

    // First match
    let release1 = make_release(
        "Brandon Sanderson - Mistborn The Final Empire (epub)",
        "guid-keep-upgrade-1",
        Some(source_id),
    );
    process_release(
        &server.app_state,
        &source,
        &release1,
        std::slice::from_ref(&pattern),
    )
    .await;

    // Second match (different guid)
    let release2 = make_release(
        "Brandon Sanderson - Mistborn The Final Empire (mobi)",
        "guid-keep-upgrade-2",
        Some(source_id),
    );
    process_release(&server.app_state, &source, &release2, &[pattern]).await;

    let matches = watch_repo::list_matches_by_pattern(pool, pattern_id, 10)
        .await
        .expect("list matches");
    assert_eq!(matches.len(), 2);

    // Confirm import for both
    for m in &matches {
        if let Some(dl_id) = m.download_id {
            confirm_import(pool, dl_id).await;
        }
    }

    // Set upgrade_until
    let until = OffsetDateTime::now_utc() + Duration::hours(720);
    watch_repo::set_upgrade_until(pool, pattern_id, until)
        .await
        .expect("set upgrade_until");

    // Reject only the first match
    watch_repo::reject_match(pool, matches[0].id, Some("wrong content"))
        .await
        .expect("reject match");

    // upgrade_until should still be set because the second import is still valid
    let pat = watch_repo::get_pattern_by_id(pool, pattern_id)
        .await
        .expect("query")
        .expect("pattern");
    assert!(
        pat.upgrade_until.is_some(),
        "upgrade_until should remain when other confirmed imports exist"
    );

    server.cleanup().await;
}

/// bulk_reject_matches should also clear upgrade_until when appropriate.
#[tokio::test]
async fn test_bulk_reject_clears_upgrade_until() {
    let (server, _mock_dl, _mock_idx) = start_mock_server().await;
    let pool = &server.pool;

    seed_download_client(pool).await;
    let source_id = seed_indexer_source(pool).await;
    let media_type_id = get_ebooks_media_type_id(pool).await;

    let (pattern_id, pattern) = create_test_pattern(
        pool,
        media_type_id,
        "http://localhost:1111/callback",
        vec!["Mistborn The Final Empire".to_string()],
        0.5,
        true,
    )
    .await;

    let source = watch_repo::get_indexer_source_by_id(pool, source_id)
        .await
        .expect("query")
        .expect("source row");

    let release = make_release(
        "Brandon Sanderson - Mistborn The Final Empire (epub)",
        "guid-bulk-reject",
        Some(source_id),
    );
    process_release(&server.app_state, &source, &release, &[pattern]).await;

    let matches = watch_repo::list_matches_by_pattern(pool, pattern_id, 10)
        .await
        .expect("list matches");
    assert_eq!(matches.len(), 1);
    let download_id = matches[0].download_id.expect("should have download");

    confirm_import(pool, download_id).await;

    let until = OffsetDateTime::now_utc() + Duration::hours(720);
    watch_repo::set_upgrade_until(pool, pattern_id, until)
        .await
        .expect("set upgrade_until");

    // Bulk reject
    let rejected = watch_repo::bulk_reject_matches(pool, &[matches[0].id], Some("bulk reject"))
        .await
        .expect("bulk reject");
    assert_eq!(rejected, 1);

    // upgrade_until should be cleared
    let pat = watch_repo::get_pattern_by_id(pool, pattern_id)
        .await
        .expect("query")
        .expect("pattern");
    assert!(
        pat.upgrade_until.is_none(),
        "upgrade_until should be cleared after bulk-rejecting the only confirmed import"
    );

    server.cleanup().await;
}
