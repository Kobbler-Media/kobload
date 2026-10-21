//! Tests for batch selection and quality rejection in the watch service.
//!
//! Covers:
//! - Quality hard-reject: 2160p release skipped when profile max is 1080p
//! - Batch picks single best: multiple releases → one match
//! - Cross-source selection: usenet preferred over torrent
//! - Cancel held matches: download cancels stale holds
//! - Realistic TV release names from production scenarios

use crate::common;

use std::sync::Arc;

use serde_json::json;
use uuid::Uuid;

use common::{
    MockDownloadClient, MockDownloadClientResolver, MockIndexer, MockIndexerResolver, TestServer,
    TestServerOptions,
};
use kobload_api::repositories::{clients as clients_repo, watch as watch_repo};
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

async fn seed_download_client(pool: &sqlx::PgPool, protocol: &str) -> Uuid {
    clients_repo::create_client(
        pool,
        clients_repo::DownloadClientInput {
            name: format!("Mock {} Client", protocol),
            client_type: "mock".to_string(),
            protocol: protocol.to_string(),
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

async fn get_tv_media_type_id(pool: &sqlx::PgPool) -> Uuid {
    let row: (Uuid,) = sqlx::query_as("SELECT id FROM media_type_config WHERE key = 'tv' LIMIT 1")
        .fetch_one(pool)
        .await
        .expect("tv media type must exist");
    row.0
}

/// Seed default quality profiles (includes "TV - 1080p" with max_resolution=1080p).
async fn seed_quality_profiles(pool: &sqlx::PgPool) {
    kobload_api::startup::ensure_default_quality_profiles(pool)
        .await
        .expect("seed quality profiles");
}

fn make_tv_release(title: &str, guid: &str, protocol: &str, source_id: Option<Uuid>) -> Release {
    Release {
        guid: guid.to_string(),
        title: title.to_string(),
        file_name: None,
        download_url: Some("http://example.com/download/123".to_string()),
        magnet_url: if protocol == "torrent" {
            Some("magnet:?xt=urn:btih:abc123".to_string())
        } else {
            None
        },
        info_url: None,
        size: 1_500_000_000,
        publish_date: "2025-03-01".to_string(),
        category_ids: vec![5000], // TV
        indexer: "mock-indexer".to_string(),
        protocol: protocol.to_string(),
        info_hash: if protocol == "torrent" {
            Some("abc123def456".to_string())
        } else {
            None
        },
        files: None,
        seeders: if protocol == "torrent" {
            Some(50)
        } else {
            None
        },
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

/// Create a TV watch pattern with match_targets for a show.
async fn create_tv_pattern(
    pool: &sqlx::PgPool,
    media_type_id: Uuid,
    show_name: &str,
    season: &str,
    episode: &str,
) -> (Uuid, watch_repo::WatchPatternRow) {
    let source_id = Uuid::now_v7().to_string();
    let target = format!("{} S{}E{}", show_name, season, episode);
    let pattern_id = watch_repo::create_pattern(
        pool,
        "test",
        &source_id,
        watch_repo::WatchPatternInput {
            callback_url: "http://localhost:1111/callback".to_string(),
            patterns: vec![],
            media_want: Some(json!({
                "TvEpisode": {
                    "show_name": show_name,
                    "season": season,
                    "episode": episode,
                    "identifiers": null
                }
            })),
            match_targets: Some(json!([
                {
                    "id": "target-1",
                    "target": target,
                }
            ])),
            match_mode: Some("fuzzy".to_string()),
            min_score: Some(0.6),
            media_type_id: Some(media_type_id),
            auto_download: true,
            metadata: None,
            enabled: true,
            upgrade_allowed: false,
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

/// Quality hard-reject: a 2160p release should be silently skipped when the
/// default TV profile has max_resolution=1080p. No match row created.
///
/// Mirrors the production DTF St. Louis scenario where 2160p torrents were
/// matched despite max_resolution being 1080p.
#[tokio::test]
async fn test_quality_rejects_2160p_with_1080p_max_profile() {
    let (server, _mock_dl, _mock_idx) = start_mock_server().await;
    let pool = &server.pool;

    seed_download_client(pool, "usenet").await;
    let source_id = seed_indexer_source(pool).await;
    let media_type_id = get_tv_media_type_id(pool).await;
    seed_quality_profiles(pool).await;

    let (pattern_id, pattern) =
        create_tv_pattern(pool, media_type_id, "DTF St. Louis", "01", "02").await;

    let source = watch_repo::get_indexer_source_by_id(pool, source_id)
        .await
        .expect("query")
        .expect("source row");

    // 2160p release — should be rejected by TV - 1080p profile (max_resolution=1080p)
    let release = make_tv_release(
        "DTF.St.Louis.S01E02.2160p.WEB-DL.DDP5.1.x265-GroupA",
        "guid-4k-001",
        "usenet",
        Some(source_id),
    );

    process_release(&server.app_state, &source, &release, &[pattern]).await;

    let matches = watch_repo::list_matches_by_pattern(pool, pattern_id, 10)
        .await
        .expect("list matches");
    assert!(
        matches.is_empty(),
        "2160p release should be quality-rejected with 1080p max profile — got {} matches",
        matches.len()
    );

    server.cleanup().await;
}

/// Quality accepts 1080p release when max_resolution is 1080p.
#[tokio::test]
async fn test_quality_accepts_1080p_within_profile() {
    let (server, _mock_dl, _mock_idx) = start_mock_server().await;
    let pool = &server.pool;

    seed_download_client(pool, "usenet").await;
    let source_id = seed_indexer_source(pool).await;
    let media_type_id = get_tv_media_type_id(pool).await;
    seed_quality_profiles(pool).await;

    let (pattern_id, pattern) =
        create_tv_pattern(pool, media_type_id, "DTF St. Louis", "01", "02").await;

    let source = watch_repo::get_indexer_source_by_id(pool, source_id)
        .await
        .expect("query")
        .expect("source row");

    let release = make_tv_release(
        "DTF.St.Louis.S01E02.1080p.WEB-DL.DDP5.1.x265-GroupB",
        "guid-1080p-001",
        "usenet",
        Some(source_id),
    );

    process_release(&server.app_state, &source, &release, &[pattern]).await;

    let matches = watch_repo::list_matches_by_pattern(pool, pattern_id, 10)
        .await
        .expect("list matches");
    assert_eq!(matches.len(), 1, "1080p should be accepted");
    assert_eq!(matches[0].action, "downloaded");

    server.cleanup().await;
}

/// Quality also rejects 720p when min_resolution is set.
/// The default TV profile has no min_resolution, so we use a custom profile.
#[tokio::test]
async fn test_quality_rejects_480p_with_remux_profile() {
    let (server, _mock_dl, _mock_idx) = start_mock_server().await;
    let pool = &server.pool;

    seed_download_client(pool, "usenet").await;
    let source_id = seed_indexer_source(pool).await;
    let media_type_id = get_tv_media_type_id(pool).await;
    seed_quality_profiles(pool).await;

    // Set the "TV - Remux" profile as default (has min_resolution=1080p)
    // First, unset current default
    sqlx::query("UPDATE quality_profiles SET is_default = false WHERE media_kind = 'tv'")
        .execute(pool)
        .await
        .unwrap();
    sqlx::query(
        "UPDATE quality_profiles SET is_default = true WHERE name = 'TV - Remux' AND media_kind = 'tv'",
    )
    .execute(pool)
    .await
    .unwrap();

    let (pattern_id, pattern) =
        create_tv_pattern(pool, media_type_id, "Test Show", "01", "01").await;

    let source = watch_repo::get_indexer_source_by_id(pool, source_id)
        .await
        .expect("query")
        .expect("source row");

    // 480p release — below min_resolution of 1080p
    let release = make_tv_release(
        "Test.Show.S01E01.480p.XviD-GroupC",
        "guid-480p-001",
        "usenet",
        Some(source_id),
    );

    process_release(&server.app_state, &source, &release, &[pattern]).await;

    let matches = watch_repo::list_matches_by_pattern(pool, pattern_id, 10)
        .await
        .expect("list matches");
    assert!(
        matches.is_empty(),
        "480p should be rejected with min_resolution=1080p"
    );

    server.cleanup().await;
}

/// Batch selection: when multiple releases are scored in one batch, only the
/// single best per (pattern_id, episode_key) gets selected.
///
/// Tests `process_releases` which feeds all releases into `score_source_releases`
/// at once, using the keep-best-per-group HashMap. Only one winner is sent to
/// `select_and_execute_winners`.
#[tokio::test]
async fn test_batch_picks_single_best_from_multiple_releases() {
    use kobload_api::watch::process_releases;

    let (server, _mock_dl, _mock_idx) = start_mock_server().await;
    let pool = &server.pool;

    seed_download_client(pool, "usenet").await;
    let source_id = seed_indexer_source(pool).await;
    let media_type_id = get_tv_media_type_id(pool).await;
    seed_quality_profiles(pool).await;

    let (pattern_id, pattern) =
        create_tv_pattern(pool, media_type_id, "DTF St. Louis", "01", "02").await;

    let source = watch_repo::get_indexer_source_by_id(pool, source_id)
        .await
        .expect("query")
        .expect("source row");

    // 4 releases from one RSS scan — only the best should win
    let releases = vec![
        make_tv_release(
            "DTF.St.Louis.S01E02.1080p.WEB-DL.DDP5.1.x265-GroupA",
            "guid-batch-best",
            "usenet",
            Some(source_id),
        ),
        make_tv_release(
            "DTF.St.Louis.S01E02.720p.HDTV.x264-GroupB",
            "guid-batch-720p",
            "usenet",
            Some(source_id),
        ),
        make_tv_release(
            "DTF.St.Louis.S01E02.480p.XviD-GroupC",
            "guid-batch-480p",
            "usenet",
            Some(source_id),
        ),
        make_tv_release(
            "DTF.St.Louis.S01E02.1080p.BluRay.x264-GroupD",
            "guid-batch-bluray",
            "usenet",
            Some(source_id),
        ),
    ];

    // Process all releases in one batch
    process_releases(&server.app_state, &source, &releases, &[pattern]).await;

    let matches = watch_repo::list_matches_by_pattern(pool, pattern_id, 10)
        .await
        .expect("list matches");
    assert_eq!(
        matches.len(),
        1,
        "batch should pick exactly 1 winner from 4 releases — got {}",
        matches.len()
    );
    assert_eq!(matches[0].action, "downloaded");

    server.cleanup().await;
}

/// Multi-pattern RSS: a release that only matches one of two active patterns
/// must produce exactly one match, and the non-matching pattern must be left
/// untouched.
///
/// This is the regression guard for the koblem cross-pattern contamination
/// bug: before the fix, scoring pooled ProcessedReleases across every pattern
/// into one flat Vec, and a single koblem review request was fired per winning
/// pattern with the whole pool attached. After the fix, per-pattern scoring
/// stays isolated in `processed_by_pattern` and each pattern's koblem review
/// (if fired) sees only its own scored rows. NATS isn't mocked in tests so
/// we can't observe the review publish directly — this test validates that
/// the refactored threading of the HashMap through `process_releases` doesn't
/// regress kobload's per-pattern matching behaviour.
#[tokio::test]
async fn test_multi_pattern_rss_isolates_scoring_per_pattern() {
    use kobload_api::watch::process_releases;

    let (server, _mock_dl, _mock_idx) = start_mock_server().await;
    let pool = &server.pool;

    seed_download_client(pool, "usenet").await;
    let source_id = seed_indexer_source(pool).await;
    let media_type_id = get_tv_media_type_id(pool).await;
    seed_quality_profiles(pool).await;

    let (euphoria_id, euphoria_pattern) =
        create_tv_pattern(pool, media_type_id, "Euphoria", "03", "02").await;
    let (severance_id, severance_pattern) =
        create_tv_pattern(pool, media_type_id, "Severance", "02", "07").await;

    let source = watch_repo::get_indexer_source_by_id(pool, source_id)
        .await
        .expect("query")
        .expect("source row");

    // One release that matches only Euphoria. If pooling were broken, the
    // Severance pattern would still be evaluating this release in its own
    // score_release pass — the test asserts kobload's per-pattern match output
    // stays clean regardless.
    let release = make_tv_release(
        "Euphoria.S03E02.Episode.2.1080p.AMZN.WEB-DL.DDP5.1.Atmos.H.264-N1H4L",
        "guid-multi-pattern-euphoria",
        "usenet",
        Some(source_id),
    );

    process_releases(
        &server.app_state,
        &source,
        std::slice::from_ref(&release),
        &[euphoria_pattern, severance_pattern],
    )
    .await;

    let euphoria_matches = watch_repo::list_matches_by_pattern(pool, euphoria_id, 10)
        .await
        .expect("list euphoria matches");
    assert_eq!(
        euphoria_matches.len(),
        1,
        "Euphoria pattern should have matched the release exactly once",
    );
    assert_eq!(euphoria_matches[0].action, "downloaded");

    let severance_matches = watch_repo::list_matches_by_pattern(pool, severance_id, 10)
        .await
        .expect("list severance matches");
    assert!(
        severance_matches.is_empty(),
        "Severance pattern must not match a Euphoria release — got {} matches",
        severance_matches.len(),
    );

    server.cleanup().await;
}

/// Quality rejection filters out bad releases BEFORE batch selection.
/// A mix of 2160p (rejected) and 1080p (accepted) releases from the same
/// RSS scan should result in only the 1080p match.
#[tokio::test]
async fn test_quality_rejection_filters_before_batch_selection() {
    let (server, _mock_dl, _mock_idx) = start_mock_server().await;
    let pool = &server.pool;

    seed_download_client(pool, "usenet").await;
    let source_id = seed_indexer_source(pool).await;
    let media_type_id = get_tv_media_type_id(pool).await;
    seed_quality_profiles(pool).await;

    let (pattern_id, pattern) =
        create_tv_pattern(pool, media_type_id, "DTF St. Louis", "01", "02").await;

    let source = watch_repo::get_indexer_source_by_id(pool, source_id)
        .await
        .expect("query")
        .expect("source row");

    // 2160p release — should be quality-rejected
    let release_4k = make_tv_release(
        "DTF.St.Louis.S01E02.2160p.WEB-DL.DDP5.1.x265-GroupA",
        "guid-filter-4k",
        "usenet",
        Some(source_id),
    );

    // 1080p release — should pass quality gate
    let release_1080p = make_tv_release(
        "DTF.St.Louis.S01E02.1080p.WEB-DL.DDP5.1.x265-GroupB",
        "guid-filter-1080p",
        "usenet",
        Some(source_id),
    );

    // Process 4k first — should be rejected, no match
    process_release(
        &server.app_state,
        &source,
        &release_4k,
        std::slice::from_ref(&pattern),
    )
    .await;

    // Process 1080p — should create match
    process_release(&server.app_state, &source, &release_1080p, &[pattern]).await;

    let matches = watch_repo::list_matches_by_pattern(pool, pattern_id, 10)
        .await
        .expect("list matches");
    assert_eq!(matches.len(), 1, "only 1080p should match");
    assert!(
        matches[0].release_title.contains("1080p"),
        "match should be the 1080p release, got: {}",
        matches[0].release_title
    );

    server.cleanup().await;
}

/// Cancel held matches: when a download is created, any existing held matches
/// for the same pattern should be cancelled.
#[tokio::test]
async fn test_download_cancels_held_matches() {
    let (server, _mock_dl, _mock_idx) = start_mock_server().await;
    let pool = &server.pool;

    seed_download_client(pool, "usenet").await;
    let source_id = seed_indexer_source(pool).await;
    let media_type_id = get_tv_media_type_id(pool).await;

    let (pattern_id, pattern) =
        create_tv_pattern(pool, media_type_id, "DTF St. Louis", "01", "02").await;

    // Manually insert 3 held matches (simulating torrent holds from previous cycles)
    let hold_until = time::OffsetDateTime::now_utc() + time::Duration::hours(1);
    for i in 1..=3 {
        watch_repo::create_match(
            pool,
            &watch_repo::WatchMatchInput {
                pattern_id,
                indexer_source_id: Some(source_id),
                release_guid: &format!("held-guid-{}", i),
                release_title: &format!("DTF.St.Louis.S01E02.1080p.WEB-DL.x265-TorrentGroup{}", i),
                release_data: Some(&json!({})),
                match_score: 0.85,
                matched_pattern: "DTF St. Louis S01E02",
                action: "held",
                download_id: None,
                hold_until: Some(hold_until),
                held: true,
                quality_score: Some(0.3),
                rejected: false,
                rejected_at: None,
                rejected_reason: None,
                episode_key: None,
            },
        )
        .await
        .expect("create held match");
    }

    // Verify 3 held matches exist
    let matches_before = watch_repo::list_matches_by_pattern(pool, pattern_id, 10)
        .await
        .expect("list matches");
    assert_eq!(matches_before.len(), 3);
    assert!(matches_before.iter().all(|m| m.held));

    // Now a usenet release comes in and triggers a download
    let source = watch_repo::get_indexer_source_by_id(pool, source_id)
        .await
        .expect("query")
        .expect("source row");

    let usenet_release = make_tv_release(
        "DTF.St.Louis.S01E02.1080p.WEB-DL.DDP5.1.x265-UsenetGroup",
        "guid-usenet-winner",
        "usenet",
        Some(source_id),
    );

    process_release(&server.app_state, &source, &usenet_release, &[pattern]).await;

    // Check: should have 4 total (3 old + 1 new), but the 3 old should no longer be held
    let matches_after = watch_repo::list_matches_by_pattern(pool, pattern_id, 10)
        .await
        .expect("list matches");
    assert_eq!(matches_after.len(), 4, "should have 3 old + 1 new match");

    let held_count = matches_after.iter().filter(|m| m.held).count();
    assert_eq!(
        held_count, 0,
        "all held matches should be cancelled after download — {} still held",
        held_count
    );

    let downloaded = matches_after.iter().find(|m| m.action == "downloaded");
    assert!(downloaded.is_some(), "should have a downloaded match");
    assert_eq!(
        downloaded.unwrap().release_guid,
        "guid-usenet-winner",
        "downloaded match should be the usenet release"
    );

    server.cleanup().await;
}

/// Hardcoded subs rejection: the default TV - 1080p profile has
/// reject_hardcoded_subs=true, so HC releases should be filtered out.
#[tokio::test]
async fn test_quality_rejects_hardcoded_subs() {
    let (server, _mock_dl, _mock_idx) = start_mock_server().await;
    let pool = &server.pool;

    seed_download_client(pool, "usenet").await;
    let source_id = seed_indexer_source(pool).await;
    let media_type_id = get_tv_media_type_id(pool).await;
    seed_quality_profiles(pool).await;

    let (pattern_id, pattern) =
        create_tv_pattern(pool, media_type_id, "Test Show", "01", "01").await;

    let source = watch_repo::get_indexer_source_by_id(pool, source_id)
        .await
        .expect("query")
        .expect("source row");

    let release = make_tv_release(
        "Test.Show.S01E01.1080p.WEB-DL.HC.x265-GroupHC",
        "guid-hc-001",
        "usenet",
        Some(source_id),
    );

    process_release(&server.app_state, &source, &release, &[pattern]).await;

    let matches = watch_repo::list_matches_by_pattern(pool, pattern_id, 10)
        .await
        .expect("list matches");
    assert!(
        matches.is_empty(),
        "hardcoded subs release should be quality-rejected"
    );

    server.cleanup().await;
}

/// Remux rejection: default TV - 1080p profile has reject_remux=true.
#[tokio::test]
async fn test_quality_rejects_remux() {
    let (server, _mock_dl, _mock_idx) = start_mock_server().await;
    let pool = &server.pool;

    seed_download_client(pool, "usenet").await;
    let source_id = seed_indexer_source(pool).await;
    let media_type_id = get_tv_media_type_id(pool).await;
    seed_quality_profiles(pool).await;

    let (pattern_id, pattern) =
        create_tv_pattern(pool, media_type_id, "Test Show", "01", "01").await;

    let source = watch_repo::get_indexer_source_by_id(pool, source_id)
        .await
        .expect("query")
        .expect("source row");

    let release = make_tv_release(
        "Test.Show.S01E01.1080p.BluRay.REMUX.AVC.TrueHD.Atmos.7.1-GroupR",
        "guid-remux-001",
        "usenet",
        Some(source_id),
    );

    process_release(&server.app_state, &source, &release, &[pattern]).await;

    let matches = watch_repo::list_matches_by_pattern(pool, pattern_id, 10)
        .await
        .expect("list matches");
    assert!(
        matches.is_empty(),
        "REMUX release should be quality-rejected when reject_remux=true"
    );

    server.cleanup().await;
}

/// Unit tests for score_quality rejection flags.
#[cfg(test)]
mod quality_unit_tests {
    use kobload_api::quality::score_quality;
    use kobload_api::repositories::quality_profiles::QualityProfileRow;

    /// Build a minimal profile for testing.
    fn tv_1080p_profile() -> QualityProfileRow {
        QualityProfileRow {
            id: uuid::Uuid::new_v4(),
            name: "TV - 1080p".to_string(),
            media_kind: "tv".to_string(),
            is_default: true,
            min_resolution: None,
            max_resolution: Some("1080p".to_string()),
            preferred_sources: vec!["web-dl".to_string()],
            preferred_codecs: vec!["x265".to_string(), "x264".to_string()],
            preferred_audio: vec!["aac".to_string(), "ddp".to_string()],
            preferred_languages: vec![],
            rejected_languages: vec![],
            rejected_terms: vec![],
            rejected_sources: vec!["cam".to_string(), "ts".to_string(), "tc".to_string()],
            reject_hardcoded_subs: true,
            use_trash_filters: true,
            reject_remux: true,
            upgrade_cutoff_score: None,
            created_at: time::OffsetDateTime::now_utc(),
            updated_at: time::OffsetDateTime::now_utc(),
        }
    }

    #[test]
    fn test_score_quality_rejects_2160p() {
        let profile = tv_1080p_profile();
        let result = score_quality(
            "Show.S01E02.2160p.WEB-DL.DDP5.1.x265-Group",
            Some(&profile),
            None,
            None,
        );
        assert!(result.rejected, "2160p should be rejected");
        assert_eq!(
            result.breakdown.rejected_reason.as_deref(),
            Some("exceeds_max_resolution")
        );
    }

    #[test]
    fn test_score_quality_accepts_1080p() {
        let profile = tv_1080p_profile();
        let result = score_quality(
            "Show.S01E02.1080p.WEB-DL.DDP5.1.x265-Group",
            Some(&profile),
            None,
            None,
        );
        assert!(!result.rejected, "1080p should be accepted");
        assert!(
            result.score > 0.0,
            "1080p WEB-DL x265 should score positively"
        );
    }

    #[test]
    fn test_score_quality_accepts_720p() {
        let profile = tv_1080p_profile();
        let result = score_quality(
            "Show.S01E02.720p.HDTV.x264-Group",
            Some(&profile),
            None,
            None,
        );
        assert!(
            !result.rejected,
            "720p should be accepted (below max, no min)"
        );
    }

    #[test]
    fn test_score_quality_rejects_cam() {
        let profile = tv_1080p_profile();
        let result = score_quality("Show.S01E02.CAM.x264-Group", Some(&profile), None, None);
        assert!(result.rejected, "CAM source should be rejected");
        assert_eq!(
            result.breakdown.rejected_reason.as_deref(),
            Some("rejected_source")
        );
    }

    #[test]
    fn test_score_quality_rejects_hardcoded_subs() {
        let profile = tv_1080p_profile();
        let result = score_quality(
            "Show.S01E02.1080p.WEB-DL.HC.x265-Group",
            Some(&profile),
            None,
            None,
        );
        assert!(result.rejected, "HC release should be rejected");
        assert_eq!(
            result.breakdown.rejected_reason.as_deref(),
            Some("rejected_hardcoded_subs")
        );
    }

    #[test]
    fn test_score_quality_rejects_remux() {
        let profile = tv_1080p_profile();
        let result = score_quality(
            "Show.S01E02.1080p.BluRay.REMUX.AVC.TrueHD-Group",
            Some(&profile),
            None,
            None,
        );
        assert!(result.rejected, "REMUX should be rejected");
        assert_eq!(
            result.breakdown.rejected_reason.as_deref(),
            Some("rejected_remux")
        );
    }

    #[test]
    fn test_score_quality_no_profile_never_rejects() {
        let result = score_quality("Show.S01E02.2160p.WEB-DL.x265-Group", None, None, None);
        assert!(!result.rejected, "no profile = no rejection");
    }

    #[test]
    fn test_score_quality_rejects_below_min_resolution() {
        let mut profile = tv_1080p_profile();
        profile.min_resolution = Some("1080p".to_string());
        profile.max_resolution = None;

        let result = score_quality("Show.S01E02.480p.XviD-Group", Some(&profile), None, None);
        assert!(result.rejected, "480p should be below min_resolution 1080p");
        assert_eq!(
            result.breakdown.rejected_reason.as_deref(),
            Some("below_min_resolution")
        );
    }

    /// Production scenario: DTF St. Louis S01E02 had 7 releases including 2x 2160p.
    /// Both should be rejected.
    #[test]
    fn test_dtf_production_scenario_quality_gates() {
        let profile = tv_1080p_profile();

        // Titles matching the production data
        let titles = [
            ("DTF.St.Louis.S01E02.2160p.WEB-DL.DDP5.1.x265-GroupA", true),
            ("DTF.St.Louis.S01E02.2160p.WEB-DL.DDP5.1.H.265-GroupB", true),
            ("DTF.St.Louis.S01E02.1080p.WEB-DL.DDP5.1.x265-GroupC", false),
            ("DTF.St.Louis.S01E02.720p.HDTV.x264-GroupD", false),
            ("DTF.St.Louis.S01E02.480p.XviD-GroupE", false), // Below max, no min → accepted
            ("DTF.St.Louis.S01E02.MP4-Mobile", false),       // No resolution → accepted
        ];

        for (title, should_reject) in titles {
            let result = score_quality(title, Some(&profile), None, None);
            assert_eq!(
                result.rejected, should_reject,
                "title '{}': expected rejected={}, got rejected={} (reason: {:?})",
                title, should_reject, result.rejected, result.breakdown.rejected_reason
            );
        }
    }
}
