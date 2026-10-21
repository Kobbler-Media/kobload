//! Tests for per-episode tracking in TvSeason watch patterns.
//!
//! Verifies:
//! - episode_key stored correctly on watch matches
//! - per-episode active/imported download checks
//! - confirm_want returns partial until all episodes covered
//! - orphan detection skips TvSeason patterns

use crate::common;

use common::TestServer;
use kobload_api::repositories::{downloads as downloads_repo, watch as watch_repo};
use reqwest::StatusCode;
use serde_json::json;
use uuid::Uuid;

/// Helper: create a download client (required FK for downloads).
async fn ensure_download_client(pool: &sqlx::PgPool) -> Uuid {
    sqlx::query_scalar(
        "INSERT INTO download_clients (id, name, client_type, protocol, base_url, enabled, is_default)
         VALUES ($1, 'TestClient', 'mock', 'usenet', 'http://test', true, true)
         ON CONFLICT DO NOTHING
         RETURNING id"
    )
    .bind(Uuid::now_v7())
    .fetch_optional(pool)
    .await
    .unwrap()
    .unwrap_or_else(|| {
        // Already exists, fetch it
        panic!("download client should be created or already exist");
    })
}

/// Helper: create a TvSeason watch pattern for testing.
async fn create_tv_season_pattern(pool: &sqlx::PgPool) -> Uuid {
    let media_want = json!({
        "kind": "tv_season",
        "show_name": "DTF St Louis",
        "season": "1",
        "year": null,
        "identifiers": null,
        "quality_profile_id": null,
        "quality_overrides": null,
        "episodes_needed": [
            {"season": "1", "episode": "1", "sort_order": 1},
            {"season": "1", "episode": "2", "sort_order": 2},
            {"season": "1", "episode": "3", "sort_order": 3}
        ]
    });

    let mt_id: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM media_type_config WHERE key = 'tv'")
            .fetch_optional(pool)
            .await
            .unwrap();

    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO watch_patterns (id, source_system, source_id, callback_url, patterns, media_want, match_mode, auto_download, enabled, upgrade_allowed, media_type_id)
         VALUES ($1, 'kobbler', 'tv_season_1', 'http://localhost/callback', '{}', $2, 'fuzzy', true, true, true, $3)"
    )
    .bind(id)
    .bind(&media_want)
    .bind(mt_id)
    .execute(pool)
    .await
    .expect("create pattern");

    id
}

/// Helper: create a download and a watch match with episode_key.
async fn insert_match_with_download(
    pool: &sqlx::PgPool,
    client_id: Uuid,
    pattern_id: Uuid,
    release_title: &str,
    episode_key: Option<&str>,
    imported: bool,
) -> (Uuid, Uuid) {
    insert_match_with_download_data(
        pool,
        client_id,
        pattern_id,
        release_title,
        episode_key,
        imported,
        json!({}),
    )
    .await
}

/// [`insert_match_with_download`] with explicit `release_data` (e.g. an
/// upgrade's `replaces_download_id`).
async fn insert_match_with_download_data(
    pool: &sqlx::PgPool,
    client_id: Uuid,
    pattern_id: Uuid,
    release_title: &str,
    episode_key: Option<&str>,
    imported: bool,
    release_data: serde_json::Value,
) -> (Uuid, Uuid) {
    // Create a real download
    let download_id = downloads_repo::create_download(
        pool,
        downloads_repo::DownloadInput {
            source: "test".to_string(),
            protocol: "usenet".to_string(),
            name: release_title.to_string(),
            download_url: format!("http://example.com/{}.nzb", release_title.replace(' ', "-")),
            download_client_id: client_id,
            callback_url: Some("http://localhost/callback".to_string()),
            metadata: Some(json!({})),
            status: "downloading".to_string(),
            ..Default::default()
        },
    )
    .await
    .expect("create download");

    let match_input = watch_repo::WatchMatchInput {
        pattern_id,
        indexer_source_id: None,
        release_guid: &format!("guid-{}", Uuid::now_v7()),
        release_title,
        release_data: Some(&release_data),
        match_score: 0.9,
        matched_pattern: "DTF St Louis S01",
        action: "downloaded",
        download_id: Some(download_id),
        hold_until: None,
        held: false,
        quality_score: Some(0.8),
        rejected: false,
        rejected_at: None,
        rejected_reason: None,
        episode_key,
    };

    let match_id = watch_repo::create_match(pool, &match_input)
        .await
        .expect("create match");

    if imported {
        // mark_match_import_confirmed uses download_id
        watch_repo::mark_match_import_confirmed(pool, download_id)
            .await
            .expect("mark imported");
    }

    (match_id, download_id)
}

// =============================================================================
// Per-Episode Download State Tests
// =============================================================================

#[tokio::test]
async fn test_episode_has_active_download() {
    let server = TestServer::start().await;
    let pool = &server.pool;
    let client_id = ensure_download_client(pool).await;
    let pattern_id = create_tv_season_pattern(pool).await;

    // No matches yet
    let has_active = watch_repo::episode_has_active_download(pool, pattern_id, "S01E01")
        .await
        .unwrap();
    assert!(!has_active, "no active download yet");

    // Add an active (non-imported) match for S01E01
    insert_match_with_download(
        pool,
        client_id,
        pattern_id,
        "DTF.St.Louis.S01E01.720p",
        Some("S01E01"),
        false,
    )
    .await;

    let has_active = watch_repo::episode_has_active_download(pool, pattern_id, "S01E01")
        .await
        .unwrap();
    assert!(has_active, "S01E01 should have active download");

    // S01E02 should NOT have active download
    let has_active = watch_repo::episode_has_active_download(pool, pattern_id, "S01E02")
        .await
        .unwrap();
    assert!(!has_active, "S01E02 should not have active download");
}

#[tokio::test]
async fn test_episode_has_imported_download() {
    let server = TestServer::start().await;
    let pool = &server.pool;
    let client_id = ensure_download_client(pool).await;
    let pattern_id = create_tv_season_pattern(pool).await;

    // Add an imported match for S01E01
    insert_match_with_download(
        pool,
        client_id,
        pattern_id,
        "DTF.St.Louis.S01E01.720p",
        Some("S01E01"),
        true,
    )
    .await;

    let has_imported = watch_repo::episode_has_imported_download(pool, pattern_id, "S01E01")
        .await
        .unwrap();
    assert!(has_imported, "S01E01 should be imported");

    let has_imported = watch_repo::episode_has_imported_download(pool, pattern_id, "S01E02")
        .await
        .unwrap();
    assert!(!has_imported, "S01E02 should not be imported");
}

#[tokio::test]
async fn test_season_pack_covers_all_episodes() {
    let server = TestServer::start().await;
    let pool = &server.pool;
    let client_id = ensure_download_client(pool).await;
    let pattern_id = create_tv_season_pattern(pool).await;

    // Add a SEASON_PACK active match
    insert_match_with_download(
        pool,
        client_id,
        pattern_id,
        "DTF.St.Louis.S01.Complete.720p",
        Some("SEASON_PACK"),
        false,
    )
    .await;

    // SEASON_PACK should make every episode show as "active"
    for ep in &["S01E01", "S01E02", "S01E03"] {
        let has_active = watch_repo::episode_has_active_download(pool, pattern_id, ep)
            .await
            .unwrap();
        assert!(has_active, "{ep} should be covered by SEASON_PACK");
    }
}

#[tokio::test]
async fn test_get_imported_episode_keys() {
    let server = TestServer::start().await;
    let pool = &server.pool;
    let client_id = ensure_download_client(pool).await;
    let pattern_id = create_tv_season_pattern(pool).await;

    // Import ep1 and ep2, leave ep3 as just downloaded
    insert_match_with_download(
        pool,
        client_id,
        pattern_id,
        "DTF.St.Louis.S01E01.720p",
        Some("S01E01"),
        true,
    )
    .await;
    insert_match_with_download(
        pool,
        client_id,
        pattern_id,
        "DTF.St.Louis.S01E02.720p",
        Some("S01E02"),
        true,
    )
    .await;
    insert_match_with_download(
        pool,
        client_id,
        pattern_id,
        "DTF.St.Louis.S01E03.720p",
        Some("S01E03"),
        false,
    )
    .await;

    let keys = watch_repo::get_imported_episode_keys(pool, pattern_id)
        .await
        .unwrap();
    assert_eq!(keys.len(), 2, "only 2 episodes imported");
    assert!(keys.contains(&"S01E01".to_string()));
    assert!(keys.contains(&"S01E02".to_string()));
    assert!(!keys.contains(&"S01E03".to_string()));
}

#[tokio::test]
async fn test_best_quality_per_episode() {
    let server = TestServer::start().await;
    let pool = &server.pool;
    let client_id = ensure_download_client(pool).await;
    let pattern_id = create_tv_season_pattern(pool).await;

    // Import ep1 with quality 0.8
    insert_match_with_download(
        pool,
        client_id,
        pattern_id,
        "DTF.St.Louis.S01E01.720p",
        Some("S01E01"),
        true,
    )
    .await;

    let best = watch_repo::get_best_imported_quality_for_episode(pool, pattern_id, "S01E01").await;
    assert!(best.is_ok());
    let best = best.unwrap();
    assert!(best.is_some(), "should have quality score");
    assert!((best.unwrap() - 0.8).abs() < 0.001);

    // S01E02 has no quality score
    let best = watch_repo::get_best_imported_quality_for_episode(pool, pattern_id, "S01E02").await;
    assert!(best.unwrap().is_none());
}

// =============================================================================
// confirm_want Season-Aware Tests
// =============================================================================

/// Helper: create a TvSeason pattern where some episodes are already in_library.
/// Episodes 1 and 2 are in_library:true, episode 3 is not.
async fn create_tv_season_pattern_with_library(pool: &sqlx::PgPool) -> Uuid {
    let media_want = json!({
        "kind": "tv_season",
        "show_name": "DTF St Louis",
        "season": "1",
        "year": null,
        "identifiers": null,
        "quality_profile_id": null,
        "quality_overrides": null,
        "episodes_needed": [
            {"season": "1", "episode": "1", "sort_order": 1, "in_library": true, "library_video_height": 1080},
            {"season": "1", "episode": "2", "sort_order": 2, "in_library": true, "library_video_height": 1080},
            {"season": "1", "episode": "3", "sort_order": 3, "in_library": false}
        ]
    });

    let mt_id: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM media_type_config WHERE key = 'tv'")
            .fetch_optional(pool)
            .await
            .unwrap();

    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO watch_patterns (id, source_system, source_id, callback_url, patterns, media_want, match_mode, auto_download, enabled, upgrade_allowed, media_type_id)
         VALUES ($1, 'kobbler', 'tv_season_lib', 'http://localhost/callback', '{}', $2, 'fuzzy', true, true, true, $3)"
    )
    .bind(id)
    .bind(&media_want)
    .bind(mt_id)
    .execute(pool)
    .await
    .expect("create pattern");

    id
}

/// When some episodes are already in_library in the media_want and the remaining
/// episode is imported via Kobload, confirm_want should treat the season as fully
/// covered and set upgrade_until (not return "partial").
#[tokio::test]
async fn test_confirm_want_counts_in_library_episodes_as_covered() {
    let server = TestServer::start().await;
    let pool = &server.pool;
    let client_id = ensure_download_client(pool).await;
    let pattern_id = create_tv_season_pattern_with_library(pool).await;

    // Only import episode 3 via Kobload — episodes 1 and 2 are in_library
    insert_match_with_download(
        pool,
        client_id,
        pattern_id,
        "DTF.St.Louis.S01E03.720p",
        Some("S01E03"),
        true,
    )
    .await;

    // Confirm should succeed — all 3 episodes are covered
    // (2 via in_library + 1 via Kobload import)
    let resp = server
        .admin_client()
        .post_json("/api/v1/want/kobbler/tv_season_lib/confirm", &json!({}))
        .await;
    resp.assert_ok();
    let body: serde_json::Value = resp.json();
    assert_eq!(
        body["status"], "upgrading",
        "should not be partial — in_library episodes count as covered"
    );
    assert!(
        body["upgrade_until"].is_string(),
        "upgrade_until should be set"
    );
}

#[tokio::test]
async fn test_confirm_want_partial_season() {
    let server = TestServer::start().await;
    let pool = &server.pool;
    let _pattern_id = create_tv_season_pattern(pool).await;

    // No episodes imported yet — confirm should return partial
    let resp = server
        .admin_client()
        .post_json("/api/v1/want/kobbler/tv_season_1/confirm", &json!({}))
        .await;
    resp.assert_ok();
    let body: serde_json::Value = resp.json();
    assert_eq!(body["status"], "partial");
    assert_eq!(body["episodes_imported"], 0);
    assert_eq!(body["episodes_total"], 3);
}

#[tokio::test]
async fn test_confirm_want_fully_covered_season() {
    let server = TestServer::start().await;
    let pool = &server.pool;
    let client_id = ensure_download_client(pool).await;
    let pattern_id = create_tv_season_pattern(pool).await;

    // Import all 3 episodes
    for ep in &["S01E01", "S01E02", "S01E03"] {
        insert_match_with_download(
            pool,
            client_id,
            pattern_id,
            &format!("DTF.St.Louis.{ep}.720p"),
            Some(ep),
            true,
        )
        .await;
    }

    // Confirm should succeed (set upgrade window)
    let resp = server
        .admin_client()
        .post_json("/api/v1/want/kobbler/tv_season_1/confirm", &json!({}))
        .await;
    resp.assert_ok();
    let body: serde_json::Value = resp.json();
    assert_eq!(body["status"], "upgrading");
    assert!(body["upgrade_until"].is_string());
}

// =============================================================================
// Orphan Cleanup Tests
// =============================================================================

#[tokio::test]
async fn test_orphan_detection_skips_tv_season() {
    let server = TestServer::start().await;
    let pool = &server.pool;
    let client_id = ensure_download_client(pool).await;
    let pattern_id = create_tv_season_pattern(pool).await;

    // Import ep1 but pattern has no upgrade_until — would normally be orphan
    insert_match_with_download(
        pool,
        client_id,
        pattern_id,
        "DTF.St.Louis.S01E01.720p",
        Some("S01E01"),
        true,
    )
    .await;

    let orphans = watch_repo::find_orphaned_patterns(pool).await.unwrap();
    let is_orphan = orphans.iter().any(|o| o.id == pattern_id);
    assert!(
        !is_orphan,
        "TvSeason pattern should NOT be detected as orphan"
    );
}

// =============================================================================
// Episode Key Extraction (integration with parse_release)
// =============================================================================

#[tokio::test]
async fn test_episode_key_extraction_various_formats() {
    use kobload_api::watch::episode_key;

    let mw = json!({"kind": "tv_season", "show_name": "Test", "season": "1"});

    // Standard format
    assert_eq!(
        episode_key::extract_episode_key("Test.S01E05.720p.WEB-DL", &mw),
        Some("S01E05".to_string())
    );

    // Multi-episode
    assert_eq!(
        episode_key::extract_episode_key("Test.S01E05E06.720p", &mw),
        Some("S01E05-E06".to_string())
    );

    // Non-TvSeason should return None
    let mw_ep = json!({"kind": "tv_episode", "show_name": "Test"});
    assert_eq!(
        episode_key::extract_episode_key("Test.S01E05.720p", &mw_ep),
        None
    );

    // all_episodes_covered
    let eps = vec![
        nats_jobs::EpisodeRef {
            season: "1".to_string(),
            episode: "1".to_string(),
            sort_order: Some(1),
            air_date: None,
            in_library: None,
            library_video_height: None,
            title: None,
        },
        nats_jobs::EpisodeRef {
            season: "1".to_string(),
            episode: "2".to_string(),
            sort_order: Some(2),
            air_date: None,
            in_library: None,
            library_video_height: None,
            title: None,
        },
    ];

    assert!(!episode_key::all_episodes_covered(&["S01E01".into()], &eps));
    assert!(episode_key::all_episodes_covered(
        &["S01E01".into(), "S01E02".into()],
        &eps
    ));
    assert!(episode_key::all_episodes_covered(
        &["SEASON_PACK".into()],
        &eps
    ));
}

// =============================================================================
// Pattern List Status Tests
// =============================================================================

async fn create_movie_pattern(pool: &sqlx::PgPool) -> Uuid {
    let pattern_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO watch_patterns (id, source_system, source_id, callback_url, patterns, media_want, match_mode, auto_download, enabled, upgrade_allowed)
         VALUES ($1, 'kobbler', 'movie_1', 'http://localhost/callback', '{}', $2, 'fuzzy', true, true, true)"
    )
    .bind(pattern_id)
    .bind(json!({"kind": "movie", "title": "The Odyssey", "year": 2026}))
    .execute(pool)
    .await
    .expect("create pattern");
    pattern_id
}

async fn list_status(server: &TestServer, pattern_id: Uuid) -> String {
    let resp = server.admin_client().get("/api/v1/watch?limit=200").await;
    resp.assert_ok();
    let body: serde_json::Value = resp.json();
    body["items"]
        .as_array()
        .expect("items should be an array")
        .iter()
        .find(|p| p["id"] == pattern_id.to_string())
        .expect("pattern should be listed")["status"]
        .as_str()
        .unwrap()
        .to_string()
}

/// Rejecting a pattern's only import must put it back to "searching" — the UI
/// showed "upgrading" for it because the list status didn't carry imports.
#[tokio::test]
async fn test_list_status_returns_to_searching_after_import_rejected() {
    let server = TestServer::start().await;
    let pool = &server.pool;
    let client_id = ensure_download_client(pool).await;

    let pattern_id = create_movie_pattern(pool).await;

    assert_eq!(list_status(&server, pattern_id).await, "searching");

    let (match_id, _) = insert_match_with_download(
        pool,
        client_id,
        pattern_id,
        "The.Odyssey.The.Real.Story.2026.1080p.WEB-DL-NYXIS",
        None,
        true,
    )
    .await;
    assert_eq!(list_status(&server, pattern_id).await, "satisfied");

    // Pattern updates are broadcast to the UI's stream with this same body;
    // it used to report "searching" here because it ignored the import.
    let resp = server
        .admin_client()
        .patch_json(
            &format!("/api/v1/watch/{pattern_id}"),
            &json!({"enabled": true}),
        )
        .await;
    resp.assert_ok();
    let body: serde_json::Value = resp.json();
    assert_eq!(body["status"], "satisfied");

    server
        .admin_client()
        .post_json(
            &format!("/api/v1/watch/matches/{match_id}/reject"),
            &json!({}),
        )
        .await
        .assert_status(StatusCode::NO_CONTENT);
    assert_eq!(list_status(&server, pattern_id).await, "searching");
}

// =============================================================================
// Import Replacement Tests
// =============================================================================

async fn match_action(pool: &sqlx::PgPool, match_id: Uuid) -> String {
    sqlx::query_scalar("SELECT action FROM watch_matches WHERE id = $1")
        .bind(match_id)
        .fetch_one(pool)
        .await
        .unwrap()
}

/// A movie upgrade replaces the earlier import, so rejecting the upgrade leaves
/// nothing imported and the pattern searches again. Previously the replaced
/// (and deleted) earlier copy still counted and kept it "upgrading".
#[tokio::test]
async fn test_new_movie_import_replaces_previous_import() {
    let server = TestServer::start().await;
    let pool = &server.pool;
    let client_id = ensure_download_client(pool).await;
    let pattern_id = create_movie_pattern(pool).await;

    let (ts_match, ts_dl) = insert_match_with_download(
        pool,
        client_id,
        pattern_id,
        "The.Odyssey.2026.1080p.TS-Custom",
        None,
        true,
    )
    .await;
    let (web_match, web_dl) = insert_match_with_download_data(
        pool,
        client_id,
        pattern_id,
        "The.Odyssey.The.Real.Story.2026.1080p.WEB-DL-NYXIS",
        None,
        true,
        json!({"replaces_download_id": ts_dl.to_string()}),
    )
    .await;

    assert_eq!(match_action(pool, ts_match).await, "replaced");
    assert_eq!(match_action(pool, web_match).await, "downloaded");
    assert_eq!(
        watch_repo::get_latest_imported_download_for_pattern(pool, pattern_id)
            .await
            .unwrap(),
        Some(web_dl)
    );

    server
        .admin_client()
        .post_json(
            &format!("/api/v1/watch/matches/{web_match}/reject"),
            &json!({}),
        )
        .await
        .assert_status(StatusCode::NO_CONTENT);
    assert!(
        !watch_repo::pattern_has_imported_download(pool, pattern_id)
            .await
            .unwrap()
    );
    assert_eq!(list_status(&server, pattern_id).await, "searching");
}

/// Replacement doesn't depend on `replaces_download_id` for single-file wants:
/// any later import replaces the earlier one.
#[tokio::test]
async fn test_movie_import_without_replaces_id_still_replaces() {
    let server = TestServer::start().await;
    let pool = &server.pool;
    let client_id = ensure_download_client(pool).await;
    let pattern_id = create_movie_pattern(pool).await;

    let (first, _) =
        insert_match_with_download(pool, client_id, pattern_id, "The.Odyssey.A", None, true).await;
    let (second, _) =
        insert_match_with_download(pool, client_id, pattern_id, "The.Odyssey.B", None, true).await;

    assert_eq!(match_action(pool, first).await, "replaced");
    assert_eq!(match_action(pool, second).await, "downloaded");
}

#[tokio::test]
async fn test_episode_import_replaces_only_same_episode() {
    let server = TestServer::start().await;
    let pool = &server.pool;
    let client_id = ensure_download_client(pool).await;
    let pattern_id = create_tv_season_pattern(pool).await;

    let (e01_old, _) = insert_match_with_download(
        pool,
        client_id,
        pattern_id,
        "DTF.St.Louis.S01E01.720p",
        Some("S01E01"),
        true,
    )
    .await;
    let (e02, _) = insert_match_with_download(
        pool,
        client_id,
        pattern_id,
        "DTF.St.Louis.S01E02.720p",
        Some("S01E02"),
        true,
    )
    .await;
    let (e01_new, _) = insert_match_with_download(
        pool,
        client_id,
        pattern_id,
        "DTF.St.Louis.S01E01.1080p",
        Some("S01E01"),
        true,
    )
    .await;
    // A pack covers E02 but doesn't say it replaces it, so E02 stays.
    let (pack, _) = insert_match_with_download(
        pool,
        client_id,
        pattern_id,
        "DTF.St.Louis.S01.1080p",
        Some("SEASON_PACK"),
        true,
    )
    .await;

    assert_eq!(match_action(pool, e01_old).await, "replaced");
    assert_eq!(match_action(pool, e01_new).await, "downloaded");
    assert_eq!(match_action(pool, e02).await, "downloaded");
    assert_eq!(match_action(pool, pack).await, "downloaded");
}

#[tokio::test]
async fn test_explicit_replaces_download_id_crosses_episode_keys() {
    let server = TestServer::start().await;
    let pool = &server.pool;
    let client_id = ensure_download_client(pool).await;
    let pattern_id = create_tv_season_pattern(pool).await;

    let (pack, pack_dl) = insert_match_with_download(
        pool,
        client_id,
        pattern_id,
        "DTF.St.Louis.S01.720p",
        Some("SEASON_PACK"),
        true,
    )
    .await;
    insert_match_with_download_data(
        pool,
        client_id,
        pattern_id,
        "DTF.St.Louis.S01E01.1080p",
        Some("S01E01"),
        true,
        json!({"replaces_download_id": pack_dl.to_string()}),
    )
    .await;

    assert_eq!(match_action(pool, pack).await, "replaced");
}

/// A confirmation arriving late for an older grab must not replace the newer
/// import that already landed.
#[tokio::test]
async fn test_late_confirmation_of_older_grab_does_not_replace_newer() {
    let server = TestServer::start().await;
    let pool = &server.pool;
    let client_id = ensure_download_client(pool).await;
    let pattern_id = create_movie_pattern(pool).await;

    let (older, older_dl) =
        insert_match_with_download(pool, client_id, pattern_id, "The.Odyssey.A", None, false).await;
    let (newer, _) =
        insert_match_with_download(pool, client_id, pattern_id, "The.Odyssey.B", None, true).await;

    watch_repo::mark_match_import_confirmed(pool, older_dl)
        .await
        .unwrap();

    assert_eq!(match_action(pool, newer).await, "downloaded");
    assert_eq!(match_action(pool, older).await, "downloaded");
}

#[tokio::test]
async fn test_rejected_import_is_not_relabelled_replaced() {
    let server = TestServer::start().await;
    let pool = &server.pool;
    let client_id = ensure_download_client(pool).await;
    let pattern_id = create_movie_pattern(pool).await;

    let (first, _) =
        insert_match_with_download(pool, client_id, pattern_id, "The.Odyssey.A", None, true).await;
    watch_repo::reject_match(pool, first, None).await.unwrap();
    insert_match_with_download(pool, client_id, pattern_id, "The.Odyssey.B", None, true).await;

    assert_eq!(match_action(pool, first).await, "rejected");
}
