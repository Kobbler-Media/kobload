//! Tests for upgrade dedup logic in both process_release (watch service) and
//! backlog_search (scheduled job).
//!
//! Ensures that after a download is imported, the system doesn't re-download
//! the same content when no quality profile exists to compare upgrades against.

mod common;

use std::sync::Arc;

use serde_json::json;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use common::{
    MockDownloadClient, MockDownloadClientResolver, MockIndexer, MockIndexerResolver, TestServer,
    TestServerOptions,
};
use kobload_api::repositories::{
    clients as clients_repo, downloads as downloads_repo, watch as watch_repo,
};
use kobload_api::watch::{Release, process_release};

// =============================================================================
// Helpers
// =============================================================================

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

async fn get_ebooks_media_type_id(pool: &sqlx::PgPool) -> Uuid {
    let row: (Uuid,) =
        sqlx::query_as("SELECT id FROM media_type_config WHERE key = 'ebooks' LIMIT 1")
            .fetch_one(pool)
            .await
            .expect("ebooks media type must exist");
    row.0
}

async fn get_tv_media_type_id(pool: &sqlx::PgPool) -> Uuid {
    let row: (Uuid,) = sqlx::query_as("SELECT id FROM media_type_config WHERE key = 'tv' LIMIT 1")
        .fetch_one(pool)
        .await
        .expect("tv media type must exist");
    row.0
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

/// Create a watch pattern with upgrade_allowed=true and return (pattern_id, row).
async fn create_upgrade_pattern(
    pool: &sqlx::PgPool,
    media_type_id: Uuid,
) -> (Uuid, watch_repo::WatchPatternRow) {
    let source_id = Uuid::now_v7().to_string();
    let pattern_id = watch_repo::create_pattern(
        pool,
        "test",
        &source_id,
        watch_repo::WatchPatternInput {
            callback_url: "http://localhost:1111/callback".to_string(),
            patterns: vec!["Mistborn The Final Empire".to_string()],
            media_want: None,
            match_targets: Some(json!([
                {
                    "id": "target-1",
                    "target": "Brandon Sanderson Mistborn The Final Empire",
                }
            ])),
            match_mode: Some("fuzzy".to_string()),
            min_score: Some(0.5),
            media_type_id: Some(media_type_id),
            auto_download: true,
            metadata: Some(json!({})),
            enabled: true,
            upgrade_allowed: true,
            search_not_before: None,
        },
    )
    .await
    .expect("create pattern");

    // Fresh patterns wait the full backlog interval before being picked up
    // (see get_patterns_due_for_backlog — COALESCE(last_backlog_search_at,
    // created_at)). These upgrade tests specifically exercise the backlog
    // scheduler, so mark the pattern as "last searched long ago" so it's
    // immediately eligible.
    sqlx::query(
        "UPDATE watch_patterns SET last_backlog_search_at = NOW() - INTERVAL '48 hours' WHERE id = $1",
    )
    .bind(pattern_id)
    .execute(pool)
    .await
    .expect("backdate last_backlog_search_at");

    let row = watch_repo::get_pattern_by_id(pool, pattern_id)
        .await
        .expect("query")
        .expect("pattern row");
    (pattern_id, row)
}

/// Simulate a completed+imported download for a pattern, with no quality_score.
/// Returns the download ID.
async fn seed_imported_download(pool: &sqlx::PgPool, pattern_id: Uuid, client_id: Uuid) -> Uuid {
    let download_id = downloads_repo::create_download(
        pool,
        downloads_repo::DownloadInput {
            source: "mock".to_string(),
            protocol: "usenet".to_string(),
            profile_id: None,
            name: "Mistborn.The.Final.Empire.epub".to_string(),
            download_url: "http://example.com/download/original".to_string(),
            download_client_id: client_id,
            external_id: Some("ext-001".to_string()),
            callback_url: Some("http://localhost:1111/callback".to_string()),
            auth_token: None,
            metadata: Some(json!({})),
            status: "completed".to_string(),
            progress: Some(1.0),
            error: None,
            release_payload: None,
            api_key_id: None,
            quality_profile_id: None,
            media_kind: None,
            submission_error: None,
        },
    )
    .await
    .expect("create download");

    // Link download to pattern via watch_match (no quality_score)
    watch_repo::create_match(
        pool,
        &watch_repo::WatchMatchInput {
            pattern_id,
            indexer_source_id: None,
            release_guid: "original-guid-001",
            release_title: "Brandon Sanderson - Mistborn The Final Empire (epub)",
            release_data: Some(&json!({})),
            match_score: 0.95,
            matched_pattern: "Mistborn The Final Empire",
            action: "downloaded",
            download_id: Some(download_id),
            hold_until: None,
            held: false,
            quality_score: None, // No quality profile — the bug trigger
            rejected: false,
            rejected_at: None,
            rejected_reason: None,
            episode_key: None,
        },
    )
    .await
    .expect("create match");

    downloads_repo::confirm_import(pool, download_id)
        .await
        .expect("confirm import");
    watch_repo::mark_match_import_confirmed(pool, download_id)
        .await
        .expect("mark match imported");

    download_id
}

// =============================================================================
// Tests: process_release (watch service path)
// =============================================================================

/// After import with no quality data + upgrade_until set, process_release should NOT
/// create a new match for the same content.
#[tokio::test]
async fn test_process_release_skips_imported_no_quality_upgrade() {
    let (server, _mock_dl, _mock_idx) = start_mock_server().await;
    let pool = &server.pool;

    let client_id = seed_download_client(pool).await;
    let source_id = seed_indexer_source(pool).await;
    let media_type_id = get_ebooks_media_type_id(pool).await;

    #[allow(unused_assignments)]
    let (pattern_id, mut pattern) = create_upgrade_pattern(pool, media_type_id).await;

    // Simulate: first download completed and imported
    seed_imported_download(pool, pattern_id, client_id).await;

    // Simulate: confirm_want set upgrade_until
    let upgrade_until = OffsetDateTime::now_utc() + Duration::hours(720);
    watch_repo::set_upgrade_until(pool, pattern_id, upgrade_until)
        .await
        .expect("set upgrade_until");

    // Re-fetch pattern to get upgrade_until
    pattern = watch_repo::get_pattern_by_id(pool, pattern_id)
        .await
        .expect("query")
        .expect("pattern row");
    assert!(pattern.upgrade_until.is_some());

    let source = watch_repo::get_indexer_source_by_id(pool, source_id)
        .await
        .expect("query")
        .expect("source row");

    // A new release appears for the same content
    let release = make_release(
        "Brandon Sanderson - Mistborn The Final Empire (epub) REPACK",
        "guid-upgrade-attempt",
        Some(source_id),
    );

    process_release(&server.app_state, &source, &release, &[pattern]).await;

    // Should NOT have created a new match — already imported, no quality data to compare
    let matches = watch_repo::list_matches_by_pattern(pool, pattern_id, 10)
        .await
        .expect("list matches");
    assert_eq!(
        matches.len(),
        1,
        "should still have only the original match, not a re-download"
    );
    assert_eq!(matches[0].release_guid, "original-guid-001");

    server.cleanup().await;
}

/// With an active (not yet imported) download and upgrade_until set,
/// process_release should wait for the active download to resolve.
#[tokio::test]
async fn test_process_release_skips_active_download_upgrade() {
    let (server, _mock_dl, _mock_idx) = start_mock_server().await;
    let pool = &server.pool;

    let client_id = seed_download_client(pool).await;
    let source_id = seed_indexer_source(pool).await;
    let media_type_id = get_ebooks_media_type_id(pool).await;

    #[allow(unused_assignments)]
    let (pattern_id, mut pattern) = create_upgrade_pattern(pool, media_type_id).await;

    // Create a download that is completed but NOT imported (mid-lifecycle)
    let download_id = downloads_repo::create_download(
        pool,
        downloads_repo::DownloadInput {
            source: "mock".to_string(),
            protocol: "usenet".to_string(),
            profile_id: None,
            name: "Mistborn.The.Final.Empire.epub".to_string(),
            download_url: "http://example.com/download/original".to_string(),
            download_client_id: client_id,
            external_id: Some("ext-002".to_string()),
            callback_url: Some("http://localhost:1111/callback".to_string()),
            auth_token: None,
            metadata: Some(json!({})),
            status: "completed".to_string(),
            progress: Some(1.0),
            error: None,
            release_payload: None,
            api_key_id: None,
            quality_profile_id: None,
            media_kind: None,
            submission_error: None,
        },
    )
    .await
    .expect("create download");

    // Link it to the pattern (no import confirmation — simulates callback pending)
    watch_repo::create_match(
        pool,
        &watch_repo::WatchMatchInput {
            pattern_id,
            indexer_source_id: None,
            release_guid: "active-guid-001",
            release_title: "Brandon Sanderson - Mistborn The Final Empire (epub)",
            release_data: Some(&json!({})),
            match_score: 0.95,
            matched_pattern: "Mistborn The Final Empire",
            action: "downloaded",
            download_id: Some(download_id),
            hold_until: None,
            held: false,
            quality_score: None,
            rejected: false,
            rejected_at: None,
            rejected_reason: None,
            episode_key: None,
        },
    )
    .await
    .expect("create match");

    // Set upgrade_until (as if confirm_want was called but import hasn't finished)
    let upgrade_until = OffsetDateTime::now_utc() + Duration::hours(720);
    watch_repo::set_upgrade_until(pool, pattern_id, upgrade_until)
        .await
        .expect("set upgrade_until");

    pattern = watch_repo::get_pattern_by_id(pool, pattern_id)
        .await
        .expect("query")
        .expect("pattern row");

    let source = watch_repo::get_indexer_source_by_id(pool, source_id)
        .await
        .expect("query")
        .expect("source row");

    let release = make_release(
        "Brandon Sanderson - Mistborn The Final Empire (epub) v2",
        "guid-while-active",
        Some(source_id),
    );

    process_release(&server.app_state, &source, &release, &[pattern]).await;

    // Should NOT match — should wait for active download to resolve
    let matches = watch_repo::list_matches_by_pattern(pool, pattern_id, 10)
        .await
        .expect("list matches");
    assert_eq!(
        matches.len(),
        1,
        "should still have only the original match, waiting for import"
    );

    server.cleanup().await;
}

/// Without upgrade_until set, an imported download with upgrade_allowed should
/// NOT trigger re-download (pattern is in pre-confirm state).
#[tokio::test]
async fn test_process_release_no_upgrade_until_with_active_download() {
    let (server, _mock_dl, _mock_idx) = start_mock_server().await;
    let pool = &server.pool;

    let client_id = seed_download_client(pool).await;
    let source_id = seed_indexer_source(pool).await;
    let media_type_id = get_ebooks_media_type_id(pool).await;

    let (pattern_id, pattern) = create_upgrade_pattern(pool, media_type_id).await;

    // Active download (not imported, no upgrade_until)
    let download_id = downloads_repo::create_download(
        pool,
        downloads_repo::DownloadInput {
            source: "mock".to_string(),
            protocol: "usenet".to_string(),
            profile_id: None,
            name: "Mistborn.epub".to_string(),
            download_url: "http://example.com/download/original".to_string(),
            download_client_id: client_id,
            external_id: Some("ext-003".to_string()),
            callback_url: Some("http://localhost:1111/callback".to_string()),
            auth_token: None,
            metadata: Some(json!({})),
            status: "downloading".to_string(),
            progress: Some(0.5),
            error: None,
            release_payload: None,
            api_key_id: None,
            quality_profile_id: None,
            media_kind: None,
            submission_error: None,
        },
    )
    .await
    .expect("create download");

    watch_repo::create_match(
        pool,
        &watch_repo::WatchMatchInput {
            pattern_id,
            indexer_source_id: None,
            release_guid: "downloading-guid",
            release_title: "Mistborn epub",
            release_data: Some(&json!({})),
            match_score: 0.9,
            matched_pattern: "Mistborn",
            action: "downloaded",
            download_id: Some(download_id),
            hold_until: None,
            held: false,
            quality_score: None,
            rejected: false,
            rejected_at: None,
            rejected_reason: None,
            episode_key: None,
        },
    )
    .await
    .expect("create match");

    // upgrade_until is NOT set (confirm_want hasn't been called yet)
    assert!(pattern.upgrade_until.is_none());

    let source = watch_repo::get_indexer_source_by_id(pool, source_id)
        .await
        .expect("query")
        .expect("source row");

    let release = make_release(
        "Brandon Sanderson - Mistborn The Final Empire (epub)",
        "guid-during-download",
        Some(source_id),
    );

    process_release(&server.app_state, &source, &release, &[pattern]).await;

    let matches = watch_repo::list_matches_by_pattern(pool, pattern_id, 10)
        .await
        .expect("list matches");
    assert_eq!(
        matches.len(),
        1,
        "should not re-download while first download is active"
    );

    server.cleanup().await;
}

/// Create a TvSeason watch pattern with upgrade_allowed=true. Used to drive
/// the per-episode upgrade path (`check_episode_upgrade`). Backdates
/// last_backlog_search_at so the backlog scheduler picks it up immediately.
async fn create_tv_season_upgrade_pattern(
    pool: &sqlx::PgPool,
    media_type_id: Uuid,
    show_name: &str,
    season: &str,
    episode: &str,
) -> (Uuid, watch_repo::WatchPatternRow) {
    let source_id = Uuid::now_v7().to_string();
    let pattern_id = watch_repo::create_pattern(
        pool,
        "test",
        &source_id,
        watch_repo::WatchPatternInput {
            callback_url: "http://localhost:1111/callback".to_string(),
            patterns: vec![show_name.to_string()],
            media_want: Some(json!({
                "kind": "tv_season",
                "show_name": show_name,
                "season": season,
                "year": null,
                "identifiers": null,
                "quality_profile_id": null,
                "quality_overrides": null,
                "pack_preference": "never",
                "episodes_needed": [
                    {"season": season, "episode": episode}
                ],
            })),
            match_targets: Some(json!([
                { "id": "target-1", "target": show_name }
            ])),
            match_mode: Some("fuzzy".to_string()),
            min_score: Some(0.5),
            media_type_id: Some(media_type_id),
            auto_download: true,
            metadata: Some(json!({})),
            enabled: true,
            upgrade_allowed: true,
            search_not_before: None,
        },
    )
    .await
    .expect("create pattern");

    sqlx::query(
        "UPDATE watch_patterns SET last_backlog_search_at = NOW() - INTERVAL '48 hours' WHERE id = $1",
    )
    .bind(pattern_id)
    .execute(pool)
    .await
    .expect("backdate last_backlog_search_at");

    let row = watch_repo::get_pattern_by_id(pool, pattern_id)
        .await
        .expect("query")
        .expect("pattern row");
    (pattern_id, row)
}

/// Production regression: episode is imported at low quality (GRACE), an
/// in-flight upgrade is downloading (first NTb), and a duplicate of the
/// in-flight release shows up from a second indexer. The duplicate must be
/// skipped — the active download IS the pending upgrade.
///
/// Bug: `check_episode_upgrade` consulted only `get_best_imported_quality_*`
/// (returning GRACE's score) and ignored the active download, so the dup's
/// score beat margin against GRACE and the system grabbed it a second time.
#[tokio::test]
async fn test_process_release_skips_dup_upgrade_while_in_flight() {
    let (server, _mock_dl, _mock_idx) = start_mock_server().await;
    let pool = &server.pool;

    let client_id = seed_download_client(pool).await;
    let source_id = seed_indexer_source(pool).await;
    let media_type_id = get_tv_media_type_id(pool).await;

    let (pattern_id, pattern) = create_tv_season_upgrade_pattern(
        pool,
        media_type_id,
        "Law and Order Special Victims Unit",
        "27",
        "20",
    )
    .await;
    let episode_key = "S27E20";

    seed_imported_episode(
        pool,
        pattern_id,
        client_id,
        episode_key,
        "Law.And.Order.SVU.S27E20.1080p.WEB.h264-GRACE",
        "guid-grace-imported",
        0.0,
    )
    .await;

    let active_download_id = downloads_repo::create_download(
        pool,
        downloads_repo::DownloadInput {
            source: "mock".to_string(),
            protocol: "usenet".to_string(),
            profile_id: None,
            name: "Law.and.Order.Special.Victims.Unit.S27E20.Old.Friends.1080p.AMZN.WEB-DL.DDP5.1.H.264-NTb".to_string(),
            download_url: "http://nzbfinder.ws/getnzb/first.nzb".to_string(),
            download_client_id: client_id,
            external_id: Some("ext-ntb-nzbfinder".to_string()),
            callback_url: Some("http://localhost:1111/callback".to_string()),
            auth_token: None,
            metadata: Some(json!({})),
            status: "downloading".to_string(),
            progress: Some(0.5),
            error: None,
            release_payload: None,
            api_key_id: None,
            quality_profile_id: None,
            media_kind: Some("tv".to_string()),
            submission_error: None,
        },
    )
    .await
    .expect("create active download");

    watch_repo::create_match(
        pool,
        &watch_repo::WatchMatchInput {
            pattern_id,
            indexer_source_id: None,
            release_guid: "guid-ntb-nzbfinder",
            release_title: "Law.and.Order.Special.Victims.Unit.S27E20.Old.Friends.1080p.AMZN.WEB-DL.DDP5.1.H.264-NTb",
            release_data: Some(&json!({})),
            match_score: 0.95,
            matched_pattern: "Law and Order Special Victims Unit",
            action: "downloaded",
            download_id: Some(active_download_id),
            hold_until: None,
            held: false,
            quality_score: Some(0.25),
            rejected: false,
            rejected_at: None,
            rejected_reason: None,
            episode_key: Some(episode_key),
        },
    )
    .await
    .expect("create active match");

    let seeded = watch_repo::list_matches_by_pattern(pool, pattern_id, 10)
        .await
        .expect("list");
    assert_eq!(
        seeded.len(),
        2,
        "precondition: 2 seeded matches (imported GRACE + active in-flight NTb)"
    );

    let source = watch_repo::get_indexer_source_by_id(pool, source_id)
        .await
        .expect("query")
        .expect("source row");

    let release = Release {
        category_ids: vec![5040],
        ..make_release(
            "Law.and.Order.Special.Victims.Unit.S27E20.Old.Friends.1080p.AMZN.WEB-DL.DDP5.1.H.264-NTb",
            "guid-ntb-drunkenslug",
            Some(source_id),
        )
    };

    process_release(&server.app_state, &source, &release, &[pattern]).await;

    let after = watch_repo::list_matches_by_pattern(pool, pattern_id, 10)
        .await
        .expect("list matches");
    assert_eq!(
        after.len(),
        2,
        "duplicate upgrade must be skipped while in-flight download exists; got {}",
        after.len()
    );

    server.cleanup().await;
}

/// Pattern-level (non-episode) variant of the dup-while-in-flight regression.
/// Drives `check_pattern_upgrade` instead of `check_episode_upgrade`. Setup
/// is calibrated to actually require the fix: imported has a real
/// `quality_score`, new release scores above margin, and a quality profile
/// resolves — so pre-fix the buggy code path reaches `check_upgrade_inner`,
/// scores the candidate as a legit upgrade, and grabs it.
#[tokio::test]
async fn test_process_release_skips_dup_upgrade_while_in_flight_pattern() {
    let (server, _mock_dl, _mock_idx) = start_mock_server().await;
    let pool = &server.pool;

    let client_id = seed_download_client(pool).await;
    let source_id = seed_indexer_source(pool).await;
    let media_type_id = get_ebooks_media_type_id(pool).await;

    let (pattern_id, pattern) = create_upgrade_pattern(pool, media_type_id).await;

    seed_imported_episode(
        pool,
        pattern_id,
        client_id,
        "",
        "Mistborn.The.Final.Empire.epub.v1",
        "guid-imported-low",
        0.05,
    )
    .await;

    let active_download_id = downloads_repo::create_download(
        pool,
        downloads_repo::DownloadInput {
            source: "mock".to_string(),
            protocol: "usenet".to_string(),
            profile_id: None,
            name: "Mistborn.The.Final.Empire.1080p.epub.v2".to_string(),
            download_url: "http://indexer-a.example.com/first.nzb".to_string(),
            download_client_id: client_id,
            external_id: Some("ext-active-pattern".to_string()),
            callback_url: Some("http://localhost:1111/callback".to_string()),
            auth_token: None,
            metadata: Some(json!({})),
            status: "downloading".to_string(),
            progress: Some(0.5),
            error: None,
            release_payload: None,
            api_key_id: None,
            quality_profile_id: None,
            media_kind: None,
            submission_error: None,
        },
    )
    .await
    .expect("create active download");

    watch_repo::create_match(
        pool,
        &watch_repo::WatchMatchInput {
            pattern_id,
            indexer_source_id: None,
            release_guid: "guid-active-from-indexer-a",
            release_title: "Brandon Sanderson - Mistborn The Final Empire 1080p (epub) v2",
            release_data: Some(&json!({})),
            match_score: 0.95,
            matched_pattern: "Mistborn The Final Empire",
            action: "downloaded",
            download_id: Some(active_download_id),
            hold_until: None,
            held: false,
            quality_score: Some(0.25),
            rejected: false,
            rejected_at: None,
            rejected_reason: None,
            episode_key: None,
        },
    )
    .await
    .expect("create active match");

    let seeded = watch_repo::list_matches_by_pattern(pool, pattern_id, 10)
        .await
        .expect("list");
    assert_eq!(seeded.len(), 2, "precondition: 2 seeded matches");

    let source = watch_repo::get_indexer_source_by_id(pool, source_id)
        .await
        .expect("query")
        .expect("source row");

    let release = make_release(
        "Brandon Sanderson - Mistborn The Final Empire 1080p (epub) v2",
        "guid-dup-from-indexer-b",
        Some(source_id),
    );

    process_release(&server.app_state, &source, &release, &[pattern]).await;

    let after = watch_repo::list_matches_by_pattern(pool, pattern_id, 10)
        .await
        .expect("list matches");
    assert_eq!(
        after.len(),
        2,
        "pattern-level: duplicate upgrade must be skipped while in-flight download exists; got {}",
        after.len()
    );

    server.cleanup().await;
}

// =============================================================================
// Tests: backlog search path (the actual fix)
// =============================================================================

/// The bug: after import with no quality data, backlog_search_pattern treated
/// the pattern as a "first grab" and re-downloaded. This test verifies the fix.
#[tokio::test]
async fn test_backlog_search_skips_imported_no_quality_upgrade() {
    use kobload_api::jobs::handlers::BacklogSearchHandler;
    use kobload_api::jobs::worker::JobHandler;
    use tokio_util::sync::CancellationToken;

    let (server, _mock_dl, mock_idx) = start_mock_server().await;
    let pool = &server.pool;

    let client_id = seed_download_client(pool).await;
    let _source_id = seed_indexer_source(pool).await;
    let media_type_id = get_ebooks_media_type_id(pool).await;

    let (pattern_id, _pattern) = create_upgrade_pattern(pool, media_type_id).await;

    // Simulate: first download completed and imported
    seed_imported_download(pool, pattern_id, client_id).await;

    // Simulate: confirm_want set upgrade_until
    let upgrade_until = OffsetDateTime::now_utc() + Duration::hours(720);
    watch_repo::set_upgrade_until(pool, pattern_id, upgrade_until)
        .await
        .expect("set upgrade_until");

    // Pattern is already eligible (create_upgrade_pattern backdates last_backlog_search_at).

    // Mock indexer returns a release that would match the pattern
    let release = make_release(
        "Brandon Sanderson - Mistborn The Final Empire (epub) PROPER",
        "guid-backlog-redownload",
        None,
    );
    mock_idx.set_search_results(vec![release]);

    // Run backlog search
    let handler = BacklogSearchHandler::new(server.app_state.clone());
    let result = handler
        .execute(Uuid::now_v7(), json!({}), CancellationToken::new())
        .await;
    assert!(
        result.is_ok(),
        "backlog search should succeed: {:?}",
        result.err()
    );

    let result_json = result.unwrap();
    assert_eq!(
        result_json["matches_found"], 0,
        "should NOT find a match — already imported, no quality data to compare"
    );

    // Verify no new match was created
    let matches = watch_repo::list_matches_by_pattern(pool, pattern_id, 10)
        .await
        .expect("list matches");
    assert_eq!(
        matches.len(),
        1,
        "should still have only the original match, not a re-download"
    );
    assert_eq!(matches[0].release_guid, "original-guid-001");

    server.cleanup().await;
}

/// Backlog search should wait when a download is active (completed but not imported).
#[tokio::test]
async fn test_backlog_search_skips_active_download_upgrade() {
    use kobload_api::jobs::handlers::BacklogSearchHandler;
    use kobload_api::jobs::worker::JobHandler;
    use tokio_util::sync::CancellationToken;

    let (server, _mock_dl, mock_idx) = start_mock_server().await;
    let pool = &server.pool;

    let client_id = seed_download_client(pool).await;
    let _source_id = seed_indexer_source(pool).await;
    let media_type_id = get_ebooks_media_type_id(pool).await;

    let (pattern_id, _pattern) = create_upgrade_pattern(pool, media_type_id).await;

    // Create a download that is completed but NOT imported
    let download_id = downloads_repo::create_download(
        pool,
        downloads_repo::DownloadInput {
            source: "mock".to_string(),
            protocol: "usenet".to_string(),
            profile_id: None,
            name: "Mistborn.epub".to_string(),
            download_url: "http://example.com/download/original".to_string(),
            download_client_id: client_id,
            external_id: Some("ext-backlog-active".to_string()),
            callback_url: Some("http://localhost:1111/callback".to_string()),
            auth_token: None,
            metadata: Some(json!({})),
            status: "completed".to_string(),
            progress: Some(1.0),
            error: None,
            release_payload: None,
            api_key_id: None,
            quality_profile_id: None,
            media_kind: None,
            submission_error: None,
        },
    )
    .await
    .expect("create download");

    watch_repo::create_match(
        pool,
        &watch_repo::WatchMatchInput {
            pattern_id,
            indexer_source_id: None,
            release_guid: "backlog-active-guid",
            release_title: "Mistborn epub",
            release_data: Some(&json!({})),
            match_score: 0.9,
            matched_pattern: "Mistborn",
            action: "downloaded",
            download_id: Some(download_id),
            hold_until: None,
            held: false,
            quality_score: None,
            rejected: false,
            rejected_at: None,
            rejected_reason: None,
            episode_key: None,
        },
    )
    .await
    .expect("create match");

    // Set upgrade_until (confirm_want called but import hasn't completed)
    let upgrade_until = OffsetDateTime::now_utc() + Duration::hours(720);
    watch_repo::set_upgrade_until(pool, pattern_id, upgrade_until)
        .await
        .expect("set upgrade_until");

    // Mock indexer returns a matching release
    let release = make_release(
        "Brandon Sanderson - Mistborn The Final Empire (epub) v2",
        "guid-backlog-while-active",
        None,
    );
    mock_idx.set_search_results(vec![release]);

    let handler = BacklogSearchHandler::new(server.app_state.clone());
    let result = handler
        .execute(Uuid::now_v7(), json!({}), CancellationToken::new())
        .await;
    assert!(result.is_ok());

    let result_json = result.unwrap();
    assert_eq!(
        result_json["matches_found"], 0,
        "should NOT match — active download should block"
    );

    let matches = watch_repo::list_matches_by_pattern(pool, pattern_id, 10)
        .await
        .expect("list matches");
    assert_eq!(
        matches.len(),
        1,
        "should still have only the original match"
    );

    server.cleanup().await;
}

/// Production regression via the `backlog_search_pattern` entry point.
/// Mirrors `test_process_release_skips_dup_upgrade_while_in_flight_pattern`
/// but driven by backlog instead of RSS — proves the shared
/// `check_pattern_upgrade` gate holds for backlog callers too.
///
/// Calls `backlog_search_pattern` directly rather than going through
/// `BacklogSearchHandler::execute`. The handler acquires
/// `pattern_search_locks` at the top of its loop; since `handle_match`
/// also try-acquires the same lock, calling via the handler short-circuits
/// any download creation regardless of upgrade outcome — making any test
/// that asserts "no dup created" pass for the wrong reason.
///
/// Note on path selection: for episode (TvSeason) patterns, the backlog
/// flow short-circuits via the "all needed episodes already satisfied"
/// early-exit before reaching `check_episode_upgrade`. The bug only
/// manifests in `check_pattern_upgrade`, which a non-season pattern with
/// a quality profile reaches.
#[tokio::test]
async fn test_backlog_search_skips_dup_upgrade_while_in_flight_pattern() {
    use kobload_api::jobs::handlers::{SearchGates, backlog_search_pattern};

    let (server, _mock_dl, mock_idx) = start_mock_server().await;
    let pool = &server.pool;

    let client_id = seed_download_client(pool).await;
    let _source_id = seed_indexer_source(pool).await;
    let media_type_id = get_ebooks_media_type_id(pool).await;

    let profile_id = {
        use kobload_api::repositories::quality_profiles as qp_repo;
        qp_repo::create_quality_profile(
            pool,
            qp_repo::QualityProfileInput {
                name: format!("test-default-ebooks-{}", Uuid::now_v7()),
                media_kind: "ebooks".to_string(),
                min_resolution: None,
                max_resolution: None,
                preferred_sources: vec![],
                rejected_sources: vec![],
                preferred_codecs: vec![],
                preferred_audio: vec![],
                preferred_languages: vec![],
                rejected_languages: vec![],
                rejected_terms: vec![],
                reject_hardcoded_subs: false,
                use_trash_filters: false,
                reject_remux: false,
                is_default: true,
                upgrade_cutoff_score: None,
            },
        )
        .await
        .expect("create default profile")
    };

    let source_id_str = Uuid::now_v7().to_string();
    let pattern_id = watch_repo::create_pattern(
        pool,
        "test",
        &source_id_str,
        watch_repo::WatchPatternInput {
            callback_url: "http://localhost:1111/callback".to_string(),
            patterns: vec!["Mistborn The Final Empire".to_string()],
            media_want: Some(json!({
                "kind": "book",
                "title": "Mistborn The Final Empire",
                "author": "Brandon Sanderson",
                "isbn": null,
                "format_preferences": ["epub"],
                "identifiers": null,
                "quality_profile_id": profile_id,
                "quality_overrides": null,
            })),
            match_targets: Some(json!([
                { "id": "t", "target": "Brandon Sanderson Mistborn The Final Empire" }
            ])),
            match_mode: Some("fuzzy".to_string()),
            min_score: Some(0.5),
            media_type_id: Some(media_type_id),
            auto_download: true,
            metadata: Some(json!({})),
            enabled: true,
            upgrade_allowed: true,
            search_not_before: None,
        },
    )
    .await
    .expect("create pattern");

    sqlx::query(
        "UPDATE watch_patterns SET last_backlog_search_at = NOW() - INTERVAL '48 hours' WHERE id = $1",
    )
    .bind(pattern_id)
    .execute(pool)
    .await
    .expect("backdate last_backlog_search_at");

    seed_imported_episode(
        pool,
        pattern_id,
        client_id,
        "",
        "Mistborn.The.Final.Empire.epub.v1",
        "guid-imported-low",
        0.05,
    )
    .await;

    let active_download_id = downloads_repo::create_download(
        pool,
        downloads_repo::DownloadInput {
            source: "mock".to_string(),
            protocol: "usenet".to_string(),
            profile_id: None,
            name: "Mistborn.The.Final.Empire.1080p.epub.v2".to_string(),
            download_url: "http://indexer-a.example.com/first.nzb".to_string(),
            download_client_id: client_id,
            external_id: Some("ext-active-backlog".to_string()),
            callback_url: Some("http://localhost:1111/callback".to_string()),
            auth_token: None,
            metadata: Some(json!({})),
            status: "downloading".to_string(),
            progress: Some(0.5),
            error: None,
            release_payload: None,
            api_key_id: None,
            quality_profile_id: None,
            media_kind: None,
            submission_error: None,
        },
    )
    .await
    .expect("create active download");

    watch_repo::create_match(
        pool,
        &watch_repo::WatchMatchInput {
            pattern_id,
            indexer_source_id: None,
            release_guid: "guid-active-from-indexer-a",
            release_title: "Brandon Sanderson - Mistborn The Final Empire 1080p (epub) v2",
            release_data: Some(&json!({})),
            match_score: 0.95,
            matched_pattern: "Mistborn The Final Empire",
            action: "downloaded",
            download_id: Some(active_download_id),
            hold_until: None,
            held: false,
            quality_score: Some(0.25),
            rejected: false,
            rejected_at: None,
            rejected_reason: None,
            episode_key: None,
        },
    )
    .await
    .expect("create active match");

    let pattern = watch_repo::get_pattern_by_id(pool, pattern_id)
        .await
        .expect("query")
        .expect("pattern row");
    assert!(pattern.upgrade_allowed);

    let release = make_release(
        "Brandon Sanderson - Mistborn The Final Empire 1080p (epub) v2",
        "guid-dup-from-indexer-b",
        None,
    );
    mock_idx.set_search_results(vec![release]);

    backlog_search_pattern(&server.app_state, &pattern, SearchGates::AUTOMATIC).await;

    let after = watch_repo::list_matches_by_pattern(pool, pattern_id, 10)
        .await
        .expect("list matches");
    assert_eq!(
        after.len(),
        2,
        "backlog: duplicate upgrade must be skipped while in-flight download exists; got {}",
        after.len()
    );

    server.cleanup().await;
}

/// Regression: a freshly-created pattern must not be picked up by the
/// periodic backlog search job on its next cycle while the want handler is
/// still running its own immediate search. Before the fix,
/// `get_patterns_due_for_backlog` matched `last_backlog_search_at IS NULL`
/// which made every brand-new pattern immediately eligible — producing the
/// observed race where a want and a backlog session both downloaded
/// different releases for the same movie 1.6 seconds apart.
///
/// After the fix: `COALESCE(last_backlog_search_at, created_at)` is used as
/// the baseline, so a fresh pattern waits the full backlog interval.
#[tokio::test]
async fn test_backlog_skips_freshly_created_pattern() {
    use kobload_api::jobs::handlers::BacklogSearchHandler;
    use kobload_api::jobs::worker::JobHandler;
    use tokio_util::sync::CancellationToken;

    let (server, _mock_dl, _mock_idx) = start_mock_server().await;
    let pool = &server.pool;

    let media_type_id = get_ebooks_media_type_id(pool).await;

    // Create a pattern via the real code path and leave last_backlog_search_at
    // as NULL (the production default after create_pattern).
    let source_id = Uuid::now_v7().to_string();
    let pattern_id = watch_repo::create_pattern(
        pool,
        "test",
        &source_id,
        watch_repo::WatchPatternInput {
            callback_url: "http://localhost:1111/callback".to_string(),
            patterns: vec!["Some Fresh Pattern".to_string()],
            media_want: None,
            match_targets: Some(json!([{"id": "t", "target": "Some Fresh Pattern"}])),
            match_mode: Some("fuzzy".to_string()),
            min_score: Some(0.5),
            media_type_id: Some(media_type_id),
            auto_download: true,
            metadata: Some(json!({})),
            enabled: true,
            upgrade_allowed: false,
            search_not_before: None,
        },
    )
    .await
    .expect("create pattern");

    // Sanity: the pattern was just created, last_backlog_search_at is NULL.
    let last_ts: Option<OffsetDateTime> =
        sqlx::query_scalar("SELECT last_backlog_search_at FROM watch_patterns WHERE id = $1")
            .bind(pattern_id)
            .fetch_one(pool)
            .await
            .expect("select last_backlog_search_at");
    assert!(
        last_ts.is_none(),
        "precondition: fresh pattern must have NULL last_backlog_search_at"
    );

    // Run the backlog handler. The fresh pattern must NOT be picked up.
    let handler = BacklogSearchHandler::new(server.app_state.clone());
    let result = handler
        .execute(Uuid::now_v7(), json!({}), CancellationToken::new())
        .await
        .expect("handler execute");
    let checked = result["patterns_checked"].as_i64().unwrap_or(-1);
    assert_eq!(
        checked, 0,
        "fresh pattern must not be picked up by the next backlog cycle"
    );

    // Now backdate created_at past the backlog interval — the pattern should
    // become eligible. (We use created_at here specifically because
    // last_backlog_search_at is still NULL, so the COALESCE fallback kicks in.)
    sqlx::query("UPDATE watch_patterns SET created_at = NOW() - INTERVAL '48 hours' WHERE id = $1")
        .bind(pattern_id)
        .execute(pool)
        .await
        .expect("backdate created_at");

    let result = handler
        .execute(Uuid::now_v7(), json!({}), CancellationToken::new())
        .await
        .expect("handler execute 2");
    let checked = result["patterns_checked"].as_i64().unwrap_or(-1);
    assert!(
        checked >= 1,
        "backdated pattern must be picked up (got patterns_checked={checked})"
    );

    server.cleanup().await;
}

/// Regression for the observed bug: a `want` trigger and a `backlog_search`
/// trigger fire concurrently for the same pattern. Before the fix they each
/// scored releases independently and dispatched a different file (two
/// downloads for one want). After the fix the per-pattern search lock
/// serialises them — whoever gets the lock first creates the download, the
/// second caller either skips (backlog try_acquire fails) or sees
/// `pattern_has_active_download = true` after acquiring.
///
/// This test drives the lock primitive directly (via the shared
/// `PatternSearchLocks` on AppState) — the full indexer-mock race is
/// exercised by the existing upgrade_dedup tests.
#[tokio::test]
async fn test_concurrent_search_lock_serialises_same_pattern() {
    use std::sync::atomic::{AtomicU32, Ordering};

    let (server, _mock_dl, _mock_idx) = start_mock_server().await;
    let locks = server.app_state.pattern_search_locks.clone();
    let pattern_id = Uuid::now_v7();

    // Blocking acquire + try_acquire from a different task: the try must fail
    // while the blocking holder is alive.
    let held = locks.acquire(pattern_id).await;
    let locks_clone = locks.clone();
    let try_result = tokio::spawn(async move { locks_clone.try_acquire(pattern_id).is_some() })
        .await
        .unwrap();
    assert!(
        !try_result,
        "try_acquire must fail while another session holds the lock"
    );
    drop(held);

    // After release, try_acquire succeeds again.
    assert!(locks.try_acquire(pattern_id).is_some());

    // Fan out 8 concurrent acquisitions on the same pattern_id, assert
    // strictly serial execution (max in-flight == 1).
    let counter = Arc::new(AtomicU32::new(0));
    let max_concurrent = Arc::new(AtomicU32::new(0));
    let mut handles = Vec::new();
    for _ in 0..8 {
        let locks = locks.clone();
        let counter = counter.clone();
        let max_concurrent = max_concurrent.clone();
        handles.push(tokio::spawn(async move {
            let _guard = locks.acquire(pattern_id).await;
            let now = counter.fetch_add(1, Ordering::SeqCst) + 1;
            max_concurrent.fetch_max(now, Ordering::SeqCst);
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            counter.fetch_sub(1, Ordering::SeqCst);
        }));
    }
    for h in handles {
        h.await.unwrap();
    }
    assert_eq!(
        max_concurrent.load(Ordering::SeqCst),
        1,
        "concurrent acquisitions on the same pattern must serialise"
    );

    // Different pattern_ids do not contend.
    let other = Uuid::now_v7();
    let a = locks.acquire(pattern_id).await;
    assert!(
        locks.try_acquire(other).is_some(),
        "distinct patterns must not contend on each other's lock"
    );
    drop(a);

    server.cleanup().await;
}

/// Regression: BacklogSearchHandler::execute acquires `pattern_search_locks`
/// at the top of its per-pattern loop, then calls `backlog_search_pattern` →
/// `handle_match`. Pre-fix, `handle_match`'s own `try_acquire` against the
/// same lock returned `None` because the same task already held it,
/// short-circuiting download creation. Net effect: the periodic backlog
/// handler could never actually grab anything — every match silently dropped.
///
/// Fresh pattern + matching release in mock indexer + no lock contention from
/// elsewhere ⇒ exactly one download/match must be created.
#[tokio::test]
async fn test_backlog_handler_creates_download_on_first_match() {
    use kobload_api::jobs::handlers::BacklogSearchHandler;
    use kobload_api::jobs::worker::JobHandler;
    use tokio_util::sync::CancellationToken;

    let (server, _mock_dl, mock_idx) = start_mock_server().await;
    let pool = &server.pool;

    let _client_id = seed_download_client(pool).await;
    let _src = seed_indexer_source(pool).await;
    let media_type_id = get_ebooks_media_type_id(pool).await;

    let (pattern_id, _pattern) = create_upgrade_pattern(pool, media_type_id).await;

    let release = make_release(
        "Brandon Sanderson - Mistborn The Final Empire (epub)",
        "guid-fresh-grab",
        None,
    );
    mock_idx.set_search_results(vec![release]);

    let handler = BacklogSearchHandler::new(server.app_state.clone());
    let result = handler
        .execute(Uuid::now_v7(), json!({}), CancellationToken::new())
        .await
        .expect("handler execute");

    assert_eq!(
        result["matches_found"], 1,
        "handler must create a download for a fresh match (got matches_found={})",
        result["matches_found"]
    );

    let matches = watch_repo::list_matches_by_pattern(pool, pattern_id, 10)
        .await
        .expect("list matches");
    assert_eq!(matches.len(), 1, "exactly one watch_match must exist");

    server.cleanup().await;
}

// =============================================================================
// Bug B: cutoff + margin gating with no time-window
// =============================================================================

/// Helper: create a quality profile with the given cutoff score.
async fn create_profile_with_cutoff(
    pool: &sqlx::PgPool,
    media_kind: &str,
    cutoff_score: Option<f64>,
) -> Uuid {
    use kobload_api::repositories::quality_profiles as qp_repo;
    qp_repo::create_quality_profile(
        pool,
        qp_repo::QualityProfileInput {
            name: format!("test-cutoff-{}", Uuid::now_v7()),
            media_kind: media_kind.to_string(),
            min_resolution: None,
            max_resolution: None,
            preferred_sources: vec![],
            rejected_sources: vec![],
            preferred_codecs: vec![],
            preferred_audio: vec![],
            preferred_languages: vec![],
            rejected_languages: vec![],
            rejected_terms: vec![],
            reject_hardcoded_subs: false,
            use_trash_filters: false,
            reject_remux: false,
            is_default: false,
            upgrade_cutoff_score: cutoff_score,
        },
    )
    .await
    .expect("create quality profile")
}

/// Helper: seed an imported episode download with the given quality_score.
async fn seed_imported_episode(
    pool: &sqlx::PgPool,
    pattern_id: Uuid,
    client_id: Uuid,
    episode_key: &str,
    release_title: &str,
    release_guid: &str,
    quality_score: f64,
) -> Uuid {
    let download_id = downloads_repo::create_download(
        pool,
        downloads_repo::DownloadInput {
            source: "mock".to_string(),
            protocol: "usenet".to_string(),
            profile_id: None,
            name: release_title.to_string(),
            download_url: format!("http://example.com/{release_guid}.nzb"),
            download_client_id: client_id,
            external_id: Some(format!("ext-{release_guid}")),
            callback_url: Some("http://localhost:1111/callback".to_string()),
            auth_token: None,
            metadata: Some(json!({})),
            status: "completed".to_string(),
            progress: Some(1.0),
            error: None,
            release_payload: None,
            api_key_id: None,
            quality_profile_id: None,
            media_kind: Some("tv".to_string()),
            submission_error: None,
        },
    )
    .await
    .expect("create download");

    watch_repo::create_match(
        pool,
        &watch_repo::WatchMatchInput {
            pattern_id,
            indexer_source_id: None,
            release_guid,
            release_title,
            release_data: Some(&json!({})),
            match_score: 0.95,
            matched_pattern: "Test Show",
            action: "downloaded",
            download_id: Some(download_id),
            hold_until: None,
            held: false,
            quality_score: Some(quality_score),
            rejected: false,
            rejected_at: None,
            rejected_reason: None,
            episode_key: Some(episode_key),
        },
    )
    .await
    .expect("create match");

    downloads_repo::confirm_import(pool, download_id)
        .await
        .expect("confirm import");
    watch_repo::mark_match_import_confirmed(pool, download_id)
        .await
        .expect("mark match imported");

    download_id
}

/// Bug A: validate the update_match_skipped helper. The reserve-then-skip
/// orchestration sites in want/search.rs and want/episode_fallback.rs
/// transition reserved rows from action='downloaded' to action='skipped'
/// via this helper instead of leaving orphans.
#[tokio::test]
async fn test_update_match_skipped_transitions_orphan_to_skipped() {
    let (server, _mock_dl, _mock_idx) = start_mock_server().await;
    let pool = &server.pool;

    let media_type_id = get_ebooks_media_type_id(pool).await;
    let (pattern_id, _pattern) = create_upgrade_pattern(pool, media_type_id).await;

    // Simulate an orphan: action='downloaded' with download_id=NULL — the
    // exact pre-fix shape that `reserve_watch_match` produced before its
    // caller transitioned the row to a final state.
    let match_id = watch_repo::create_match(
        pool,
        &watch_repo::WatchMatchInput {
            pattern_id,
            indexer_source_id: None,
            release_guid: "guid-orphan",
            release_title: "Orphan release",
            release_data: Some(&json!({})),
            match_score: 0.9,
            matched_pattern: "x",
            action: "downloaded",
            download_id: None,
            hold_until: None,
            held: false,
            quality_score: None,
            rejected: false,
            rejected_at: None,
            rejected_reason: None,
            episode_key: None,
        },
    )
    .await
    .expect("create orphan match");

    watch_repo::update_match_skipped(pool, match_id, "test reason: cutoff already met")
        .await
        .expect("update_match_skipped");

    let row = sqlx::query!(
        r#"SELECT action, rejected, rejected_reason, download_id
           FROM watch_matches WHERE id = $1"#,
        match_id
    )
    .fetch_one(pool)
    .await
    .expect("fetch updated row");

    assert_eq!(row.action, "skipped", "action must transition to 'skipped'");
    assert!(row.rejected, "rejected flag must be set");
    assert_eq!(
        row.rejected_reason.as_deref(),
        Some("test reason: cutoff already met"),
        "reason must be persisted"
    );
    assert!(row.download_id.is_none(), "download_id stays NULL");

    server.cleanup().await;
}

/// Bug A backfill regression: the migration 0075 must rewrite pre-fix
/// orphans (`action='downloaded' AND download_id IS NULL`) to 'skipped'.
/// We can't directly invoke a migration mid-test, but we can verify that
/// any orphan rows seeded by other tests have been rewritten by the time
/// the migration runs. As a proxy: insert an old-shape row, run the same
/// UPDATE the migration runs, assert the rewrite.
#[tokio::test]
async fn test_orphan_backfill_query_rewrites_pre_fix_rows() {
    let (server, _mock_dl, _mock_idx) = start_mock_server().await;
    let pool = &server.pool;

    let media_type_id = get_ebooks_media_type_id(pool).await;
    let (pattern_id, _pattern) = create_upgrade_pattern(pool, media_type_id).await;

    let match_id = watch_repo::create_match(
        pool,
        &watch_repo::WatchMatchInput {
            pattern_id,
            indexer_source_id: None,
            release_guid: "guid-pre-fix-orphan",
            release_title: "Pre-fix orphan",
            release_data: Some(&json!({})),
            match_score: 0.9,
            matched_pattern: "x",
            action: "downloaded",
            download_id: None,
            hold_until: None,
            held: false,
            quality_score: None,
            rejected: false,
            rejected_at: None,
            rejected_reason: None,
            episode_key: None,
        },
    )
    .await
    .expect("create orphan match");

    // Run the same UPDATE as migration 0075.
    sqlx::query(
        r#"UPDATE watch_matches
           SET action = 'skipped',
               rejected = TRUE,
               rejected_at = COALESCE(rejected_at, NOW()),
               rejected_reason = COALESCE(rejected_reason, 'orphaned by reserve-before-check bug (pre-fix)')
           WHERE action = 'downloaded'
             AND download_id IS NULL
             AND import_confirmed_at IS NULL
             AND created_at < NOW()
             AND id = $1"#,
    )
    .bind(match_id)
    .execute(pool)
    .await
    .expect("backfill update");

    let row = sqlx::query!(
        r#"SELECT action, rejected, rejected_reason FROM watch_matches WHERE id = $1"#,
        match_id
    )
    .fetch_one(pool)
    .await
    .expect("fetch row");

    assert_eq!(row.action, "skipped");
    assert!(row.rejected);
    assert_eq!(
        row.rejected_reason.as_deref(),
        Some("orphaned by reserve-before-check bug (pre-fix)")
    );

    server.cleanup().await;
}

/// Manual-bypass: backlog_search_pattern called with USER_REQUESTED gates must
/// allow the upgrade even when cutoff is met (the user-driven "give me this" path).
#[tokio::test]
async fn test_manual_backlog_search_bypasses_cutoff_gate() {
    use kobload_api::jobs::handlers::{SearchGates, backlog_search_pattern};

    let (server, _mock_dl, mock_idx) = start_mock_server().await;
    let pool = &server.pool;

    let client_id = seed_download_client(pool).await;
    let _src = seed_indexer_source(pool).await;
    let media_type_id = get_ebooks_media_type_id(pool).await;

    // Quality profile with cutoff_score = 70. Existing import at 0.85 (cutoff met).
    let profile_id = create_profile_with_cutoff(pool, "ebooks", Some(70.0)).await;

    let source_id = Uuid::now_v7().to_string();
    let pattern_id = watch_repo::create_pattern(
        pool,
        "test",
        &source_id,
        watch_repo::WatchPatternInput {
            callback_url: "http://localhost:1111/callback".to_string(),
            patterns: vec!["Mistborn".to_string()],
            media_want: Some(json!({
                "kind": "book",
                "title": "Mistborn",
                "author": null,
                "isbn": null,
                "format_preferences": ["epub"],
                "identifiers": null,
                "series": null,
                "series_position": null,
                "release_date": null,
                "quality_profile_id": profile_id,
                "quality_overrides": null
            })),
            match_targets: Some(json!([{"id": "t", "target": "Mistborn"}])),
            match_mode: Some("fuzzy".to_string()),
            min_score: Some(0.3),
            media_type_id: Some(media_type_id),
            auto_download: true,
            metadata: Some(json!({})),
            enabled: true,
            upgrade_allowed: true,
            search_not_before: None,
        },
    )
    .await
    .expect("create pattern");

    // Imported at 0.85 — meets cutoff.
    seed_imported_episode(
        pool,
        pattern_id,
        client_id,
        "",
        "Mistborn epub v1",
        "guid-cutoff-met",
        0.85,
    )
    .await;

    let pattern = watch_repo::get_pattern_by_id(pool, pattern_id)
        .await
        .expect("query")
        .expect("pattern");

    // Mock indexer returns a same-quality release. With USER_REQUESTED the gate
    // should be bypassed and we should at least attempt the download.
    let release = make_release(
        "Mistborn epub v2 (different release)",
        "guid-manual-replace",
        None,
    );
    mock_idx.set_search_results(vec![release]);

    // user-requested path
    let found_manual =
        backlog_search_pattern(&server.app_state, &pattern, SearchGates::USER_REQUESTED).await;
    assert!(
        found_manual,
        "USER_REQUESTED must bypass cutoff gate and grab replacement"
    );

    server.cleanup().await;
}

/// Regression: failure-driven re-search must not grab a second copy while a
/// download for the same pattern is still in flight.
///
/// The incident this guards ("The End of Oak Street", 2026-09-21): every
/// re-search after a failed download passed `manual=true`, which bypassed the
/// ONLY gate that checked for an active/imported download. One failed import
/// then walked the pattern down its whole release list — eight grabs, three of
/// them completing, two ingesting concurrently.
///
/// `SearchGates::REPLACEMENT` must bypass upgrade eligibility (so a
/// replacement that doesn't out-score the failed grab is still allowed) while
/// keeping duplicate suppression.
#[tokio::test]
async fn test_replacement_research_does_not_duplicate_active_download() {
    use kobload_api::jobs::handlers::{SearchGates, backlog_search_pattern};

    let (server, _mock_dl, mock_idx) = start_mock_server().await;
    let pool = &server.pool;

    let client_id = seed_download_client(pool).await;
    let _src = seed_indexer_source(pool).await;
    let media_type_id = get_ebooks_media_type_id(pool).await;

    let source_id = Uuid::now_v7().to_string();
    let pattern_id = watch_repo::create_pattern(
        pool,
        "test",
        &source_id,
        watch_repo::WatchPatternInput {
            callback_url: "http://localhost:1111/callback".to_string(),
            patterns: vec!["Mistborn".to_string()],
            media_want: Some(json!({
                "kind": "book",
                "title": "Mistborn",
                "author": null,
                "isbn": null,
                "format_preferences": ["epub"],
                "identifiers": null,
                "series": null,
                "series_position": null,
                "release_date": null,
                "quality_profile_id": null,
                "quality_overrides": null
            })),
            match_targets: Some(json!([{"id": "t", "target": "Mistborn"}])),
            match_mode: Some("fuzzy".to_string()),
            min_score: Some(0.3),
            media_type_id: Some(media_type_id),
            auto_download: true,
            metadata: Some(json!({})),
            enabled: true,
            // Upgrades off: this pattern just wants one good copy. Mirrors the
            // "already have it, stop grabbing" half of the incident.
            upgrade_allowed: false,
            search_not_before: None,
        },
    )
    .await
    .expect("create pattern");

    // An in-flight download for this pattern — status 'downloading', not yet imported.
    let active_id = downloads_repo::create_download(
        pool,
        downloads_repo::DownloadInput {
            source: "mock".to_string(),
            protocol: "usenet".to_string(),
            profile_id: None,
            name: "Mistborn epub v1".to_string(),
            download_url: "http://example.com/guid-active.nzb".to_string(),
            download_client_id: client_id,
            external_id: Some("ext-guid-active".to_string()),
            callback_url: Some("http://localhost:1111/callback".to_string()),
            auth_token: None,
            metadata: Some(json!({})),
            status: "downloading".to_string(),
            progress: Some(0.5),
            error: None,
            release_payload: None,
            api_key_id: None,
            quality_profile_id: None,
            media_kind: Some("book".to_string()),
            submission_error: None,
        },
    )
    .await
    .expect("create active download");

    watch_repo::create_match(
        pool,
        &watch_repo::WatchMatchInput {
            pattern_id,
            indexer_source_id: None,
            release_guid: "guid-active",
            release_title: "Mistborn epub v1",
            release_data: Some(&json!({})),
            match_score: 0.95,
            matched_pattern: "Mistborn",
            action: "downloaded",
            download_id: Some(active_id),
            hold_until: None,
            held: false,
            quality_score: Some(0.5),
            rejected: false,
            rejected_at: None,
            rejected_reason: None,
            episode_key: None,
        },
    )
    .await
    .expect("create match");

    let pattern = watch_repo::get_pattern_by_id(pool, pattern_id)
        .await
        .expect("query")
        .expect("pattern");

    let before = watch_repo::list_matches_by_pattern(pool, pattern_id, 10)
        .await
        .expect("list matches")
        .len();

    // A different release of the same title shows up, as it did in the incident.
    mock_idx.set_search_results(vec![make_release(
        "Mistborn epub v2 (different release)",
        "guid-replacement",
        None,
    )]);

    let found = backlog_search_pattern(&server.app_state, &pattern, SearchGates::REPLACEMENT).await;

    assert!(
        !found,
        "REPLACEMENT must not grab while a download for the pattern is still active"
    );

    let after = watch_repo::list_matches_by_pattern(pool, pattern_id, 10)
        .await
        .expect("list matches");
    assert_eq!(
        after.len(),
        before,
        "no new match may be created while an active download exists; got {} (was {})",
        after.len(),
        before
    );

    server.cleanup().await;
}
