//! Integration tests for TV show revival/reboot disambiguation.
//!
//! Verifies that the scoring pipeline correctly handles shows with the same name
//! but different years (e.g., Scrubs 2001 vs Scrubs 2026, MacGyver 1985 vs 2016).
//!
//! Tests exercise the full release matching flow through `process_release()` (RSS path)
//! and verify structural_reject + structural_bonus behavior end-to-end.

mod common;

use std::sync::Arc;

use serde_json::json;
use uuid::Uuid;

use common::{
    MockDownloadClient, MockDownloadClientResolver, MockIndexer, MockIndexerResolver, TestServer,
    TestServerOptions,
};
use kobload_api::repositories::{clients as clients_repo, watch as watch_repo};
use kobload_api::watch::{Release, process_release, process_releases};

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

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

async fn get_tv_media_type_id(pool: &sqlx::PgPool) -> Uuid {
    let row: (Uuid,) = sqlx::query_as("SELECT id FROM media_type_config WHERE key = 'tv' LIMIT 1")
        .fetch_one(pool)
        .await
        .expect("tv media type must exist");
    row.0
}

/// Build a release with configurable title and publish_date.
fn make_tv_release(
    title: &str,
    guid: &str,
    publish_date: &str,
    source_id: Option<Uuid>,
) -> Release {
    Release {
        guid: guid.to_string(),
        title: title.to_string(),
        file_name: None,
        download_url: Some("http://example.com/download/123".to_string()),
        magnet_url: None,
        info_url: None,
        size: 1_500_000_000,
        publish_date: publish_date.to_string(),
        category_ids: vec![5000],
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

/// Helper to compute a date string N days from today.
fn days_from_now(days: i64) -> String {
    let now = time::OffsetDateTime::now_utc().date();
    let target = now + time::Duration::days(days);
    target.to_string()
}

/// Create a TV watch pattern with a TvEpisode media_want.
#[allow(clippy::too_many_arguments)]
async fn create_tv_episode_pattern(
    pool: &sqlx::PgPool,
    media_type_id: Uuid,
    show_name: &str,
    season: &str,
    episode: &str,
    year: Option<u32>,
    air_date: Option<&str>,
    episode_title: Option<&str>,
) -> (Uuid, watch_repo::WatchPatternRow) {
    let media_want = nats_jobs::MediaWant::TvEpisode {
        show_name: show_name.to_string(),
        season: season.to_string(),
        episode: episode.to_string(),
        year,
        air_date: air_date.map(String::from),
        identifiers: None,
        alternate_titles: None,
        episode_title: episode_title.map(String::from),
        quality_profile_id: None,
        quality_overrides: None,
    };

    let source_id = Uuid::now_v7().to_string();
    let pattern_id = watch_repo::create_pattern(
        pool,
        "test",
        &source_id,
        watch_repo::WatchPatternInput {
            callback_url: "http://localhost:1111/callback".to_string(),
            patterns: vec![format!("{show_name} S{season}E{episode}")],
            media_want: Some(serde_json::to_value(&media_want).unwrap()),
            match_targets: Some(json!([{
                "id": "target-1",
                "target": format!("{show_name} S{season}E{episode}"),
            }])),
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
    .expect("create pattern");

    let row = watch_repo::get_pattern_by_id(pool, pattern_id)
        .await
        .expect("query")
        .expect("pattern row");
    (pattern_id, row)
}

/// Create a TV watch pattern with a TvSeason media_want.
async fn create_tv_season_pattern(
    pool: &sqlx::PgPool,
    media_type_id: Uuid,
    show_name: &str,
    season: &str,
    year: Option<u32>,
    episodes_needed: Vec<nats_jobs::EpisodeRef>,
    last_air_date: Option<&str>,
) -> (Uuid, watch_repo::WatchPatternRow) {
    let media_want = nats_jobs::MediaWant::TvSeason {
        show_name: show_name.to_string(),
        season: season.to_string(),
        year,
        episodes_needed: Some(episodes_needed),
        total_episodes: Some(10),
        pack_preference: "auto".to_string(),
        last_air_date: last_air_date.map(String::from),
        identifiers: None,
        alternate_titles: None,
        quality_profile_id: None,
        quality_overrides: None,
    };

    let source_id = Uuid::now_v7().to_string();
    let pattern_id = watch_repo::create_pattern(
        pool,
        "test",
        &source_id,
        watch_repo::WatchPatternInput {
            callback_url: "http://localhost:1111/callback".to_string(),
            patterns: vec![format!("{show_name} S{season}")],
            media_want: Some(serde_json::to_value(&media_want).unwrap()),
            match_targets: Some(json!([{
                "id": "target-1",
                "target": format!("{show_name} S{season}"),
            }])),
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
    .expect("create pattern");

    let row = watch_repo::get_pattern_by_id(pool, pattern_id)
        .await
        .expect("query")
        .expect("pattern row");
    (pattern_id, row)
}

fn make_episode_ref(episode: &str, air_date: Option<&str>) -> nats_jobs::EpisodeRef {
    nats_jobs::EpisodeRef {
        season: "1".to_string(),
        episode: episode.to_string(),
        sort_order: Some(episode.parse::<i32>().unwrap_or(0)),
        air_date: air_date.map(String::from),
        in_library: Some(false),
        library_video_height: None,
        title: None,
    }
}

// =============================================================================
// Air date gating tests
// =============================================================================

#[tokio::test]
async fn test_rss_rejects_unaired_episode() {
    let (server, _mock_dl, _mock_idx) = start_mock_server().await;
    let pool = &server.pool;
    seed_download_client(pool).await;
    let source_id = seed_indexer_source(pool).await;
    let media_type_id = get_tv_media_type_id(pool).await;

    let future_date = days_from_now(30);
    let (pattern_id, pattern) = create_tv_episode_pattern(
        pool,
        media_type_id,
        "Scrubs",
        "1",
        "6",
        Some(2026),
        Some(&future_date),
        None,
    )
    .await;

    let source = watch_repo::get_indexer_source_by_id(pool, source_id)
        .await
        .expect("query")
        .expect("source row");

    // Original Scrubs S01E06 — should be rejected because episode hasn't aired
    let release = make_tv_release(
        "Scrubs.S01E06.DVDRip.XviD-FoV",
        "guid-unaired-001",
        "2022-01-01",
        Some(source_id),
    );

    process_release(&server.app_state, &source, &release, &[pattern]).await;

    let matches = watch_repo::list_matches_by_pattern(pool, pattern_id, 10)
        .await
        .expect("list matches");
    assert!(
        matches.is_empty(),
        "unaired episode should not match any release"
    );

    server.cleanup().await;
}

#[tokio::test]
async fn test_rss_accepts_aired_episode() {
    let (server, _mock_dl, _mock_idx) = start_mock_server().await;
    let pool = &server.pool;
    seed_download_client(pool).await;
    let source_id = seed_indexer_source(pool).await;
    let media_type_id = get_tv_media_type_id(pool).await;

    let past_date = days_from_now(-5);
    let yesterday = days_from_now(-1);
    let (pattern_id, pattern) = create_tv_episode_pattern(
        pool,
        media_type_id,
        "Scrubs",
        "1",
        "1",
        Some(2026),
        Some(&past_date),
        None,
    )
    .await;

    let source = watch_repo::get_indexer_source_by_id(pool, source_id)
        .await
        .expect("query")
        .expect("source row");

    let release = make_tv_release(
        "Scrubs.2026.S01E01.1080p.WEB-DL.DDP5.1.H.264-NTb",
        "guid-aired-001",
        &yesterday,
        Some(source_id),
    );

    process_release(&server.app_state, &source, &release, &[pattern]).await;

    let matches = watch_repo::list_matches_by_pattern(pool, pattern_id, 10)
        .await
        .expect("list matches");
    assert_eq!(
        matches.len(),
        1,
        "aired episode with recent publish date should match"
    );

    server.cleanup().await;
}

#[tokio::test]
async fn test_rss_accepts_no_air_date() {
    let (server, _mock_dl, _mock_idx) = start_mock_server().await;
    let pool = &server.pool;
    seed_download_client(pool).await;
    let source_id = seed_indexer_source(pool).await;
    let media_type_id = get_tv_media_type_id(pool).await;

    // No air_date — graceful degradation, should not reject
    let (pattern_id, pattern) = create_tv_episode_pattern(
        pool,
        media_type_id,
        "Scrubs",
        "1",
        "1",
        Some(2026),
        None,
        None,
    )
    .await;

    let source = watch_repo::get_indexer_source_by_id(pool, source_id)
        .await
        .expect("query")
        .expect("source row");

    let yesterday = days_from_now(-1);
    let release = make_tv_release(
        "Scrubs.2026.S01E01.1080p.WEB-DL.DDP5.1.H.264-NTb",
        "guid-nodate-001",
        &yesterday,
        Some(source_id),
    );

    process_release(&server.app_state, &source, &release, &[pattern]).await;

    let matches = watch_repo::list_matches_by_pattern(pool, pattern_id, 10)
        .await
        .expect("list matches");
    assert_eq!(matches.len(), 1, "no air_date should not prevent matching");

    server.cleanup().await;
}

#[tokio::test]
async fn test_want_rejects_unaired_episode() {
    let (server, _mock_dl, mock_idx) = start_mock_server().await;
    let pool = &server.pool;
    seed_download_client(pool).await;
    seed_indexer_source(pool).await;

    let future_date = days_from_now(30);

    // Mock indexer returns original Scrubs S01E06
    let release = make_tv_release(
        "Scrubs.S01E06.DVDRip.XviD-FoV",
        "guid-want-unaired-001",
        "2022-01-01",
        None,
    );
    mock_idx.set_search_results(vec![release]);

    let resp = server
        .admin_client()
        .post_json(
            "/api/v1/want",
            &json!({
                "source_system": "kobbler",
                "source_id": format!("tv-scrubs-2026-s01e06-{}", Uuid::now_v7()),
                "callback_url": "http://localhost:1234/callback",
                "patterns": ["Scrubs S01E06"],
                "match_targets": [{"id": "target-1", "target": "Scrubs S01E06"}],
                "media_type": "tv",
                "auto_download": true,
                "min_score": 0.5,
                "media_want": {
                    "kind": "tv_episode",
                    "show_name": "Scrubs",
                    "season": "1",
                    "episode": "6",
                    "year": 2026,
                    "air_date": future_date,
                },
            }),
        )
        .await;
    resp.assert_ok();

    let body: serde_json::Value = resp.json();
    // Should NOT download — episode hasn't aired
    assert_eq!(
        body["status"], "watching",
        "unaired episode should not be downloaded, got: {body}"
    );

    server.cleanup().await;
}

// =============================================================================
// Publish date sanity tests
// =============================================================================

#[tokio::test]
async fn test_rss_rejects_old_publish_date() {
    let (server, _mock_dl, _mock_idx) = start_mock_server().await;
    let pool = &server.pool;
    seed_download_client(pool).await;
    let source_id = seed_indexer_source(pool).await;
    let media_type_id = get_tv_media_type_id(pool).await;

    let yesterday = days_from_now(-1);
    let (pattern_id, pattern) = create_tv_episode_pattern(
        pool,
        media_type_id,
        "Scrubs",
        "1",
        "1",
        Some(2026),
        Some(&yesterday),
        None,
    )
    .await;

    let source = watch_repo::get_indexer_source_by_id(pool, source_id)
        .await
        .expect("query")
        .expect("source row");

    // Release published in 2022 — way too old for a 2026 episode that aired yesterday
    let release = make_tv_release(
        "Scrubs.S01E01.DVDRip.XviD-FoV",
        "guid-old-pub-001",
        "2022-06-15",
        Some(source_id),
    );

    process_release(&server.app_state, &source, &release, &[pattern]).await;

    let matches = watch_repo::list_matches_by_pattern(pool, pattern_id, 10)
        .await
        .expect("list matches");
    assert!(
        matches.is_empty(),
        "release published years before air date should be rejected"
    );

    server.cleanup().await;
}

#[tokio::test]
async fn test_rss_accepts_recent_publish_date() {
    let (server, _mock_dl, _mock_idx) = start_mock_server().await;
    let pool = &server.pool;
    seed_download_client(pool).await;
    let source_id = seed_indexer_source(pool).await;
    let media_type_id = get_tv_media_type_id(pool).await;

    let yesterday = days_from_now(-1);
    let two_days_ago = days_from_now(-2);
    let (pattern_id, pattern) = create_tv_episode_pattern(
        pool,
        media_type_id,
        "Scrubs",
        "1",
        "1",
        Some(2026),
        Some(&yesterday),
        None,
    )
    .await;

    let source = watch_repo::get_indexer_source_by_id(pool, source_id)
        .await
        .expect("query")
        .expect("source row");

    // Release published 2 days ago, air date yesterday — recent and valid
    let release = make_tv_release(
        "Scrubs.2026.S01E01.1080p.WEB-DL.DDP5.1.H.264-NTb",
        "guid-recent-pub-001",
        &two_days_ago,
        Some(source_id),
    );

    process_release(&server.app_state, &source, &release, &[pattern]).await;

    let matches = watch_repo::list_matches_by_pattern(pool, pattern_id, 10)
        .await
        .expect("list matches");
    assert_eq!(matches.len(), 1, "recently published release should match");

    server.cleanup().await;
}

#[tokio::test]
async fn test_rss_rejects_publish_date_before_show_year() {
    let (server, _mock_dl, _mock_idx) = start_mock_server().await;
    let pool = &server.pool;
    seed_download_client(pool).await;
    let source_id = seed_indexer_source(pool).await;
    let media_type_id = get_tv_media_type_id(pool).await;

    // No air_date, but show year is 2026
    let (pattern_id, pattern) = create_tv_episode_pattern(
        pool,
        media_type_id,
        "Scrubs",
        "1",
        "1",
        Some(2026),
        None,
        None,
    )
    .await;

    let source = watch_repo::get_indexer_source_by_id(pool, source_id)
        .await
        .expect("query")
        .expect("source row");

    // Published in 2004 — before show year 2026 minus margin
    let release = make_tv_release(
        "Scrubs.S01E01.DVDRip.XviD-FoV",
        "guid-old-year-001",
        "2004-09-15",
        Some(source_id),
    );

    process_release(&server.app_state, &source, &release, &[pattern]).await;

    let matches = watch_repo::list_matches_by_pattern(pool, pattern_id, 10)
        .await
        .expect("list matches");
    assert!(
        matches.is_empty(),
        "release published years before show year should be rejected"
    );

    server.cleanup().await;
}

#[tokio::test]
async fn test_rss_accepts_publish_date_for_old_show() {
    let (server, _mock_dl, _mock_idx) = start_mock_server().await;
    let pool = &server.pool;
    seed_download_client(pool).await;
    let source_id = seed_indexer_source(pool).await;
    let media_type_id = get_tv_media_type_id(pool).await;

    // Breaking Bad S01E01 aired in 2008, show year 2008
    let (pattern_id, pattern) = create_tv_episode_pattern(
        pool,
        media_type_id,
        "Breaking Bad",
        "1",
        "1",
        Some(2008),
        Some("2008-01-20"),
        None,
    )
    .await;

    let source = watch_repo::get_indexer_source_by_id(pool, source_id)
        .await
        .expect("query")
        .expect("source row");

    // Repack published in 2024 — well after show year 2008, should be fine
    let release = make_tv_release(
        "Breaking.Bad.S01E01.720p.BluRay.x264-DEMAND",
        "guid-old-show-001",
        "2024-03-15",
        Some(source_id),
    );

    process_release(&server.app_state, &source, &release, &[pattern]).await;

    let matches = watch_repo::list_matches_by_pattern(pool, pattern_id, 10)
        .await
        .expect("list matches");
    assert_eq!(
        matches.len(),
        1,
        "repack of old show with recent publish date should match"
    );

    server.cleanup().await;
}

// =============================================================================
// Year boost tests
// =============================================================================

#[tokio::test]
async fn test_year_match_release_scores_higher() {
    // Two identical releases except for year in title.
    // The year-matching one should score higher.
    let media_want = nats_jobs::MediaWant::TvEpisode {
        show_name: "Scrubs".to_string(),
        season: "1".to_string(),
        episode: "1".to_string(),
        year: Some(2026),
        air_date: Some(days_from_now(-5)),
        identifiers: None,
        alternate_titles: None,
        episode_title: None,
        quality_profile_id: None,
        quality_overrides: None,
    };

    let yesterday = days_from_now(-1);

    let release_with_year = make_tv_release(
        "Scrubs.2026.S01E01.1080p.WEB-DL.DDP5.1.H.264-NTb",
        "guid-year-001",
        &yesterday,
        None,
    );
    let release_without_year = make_tv_release(
        "Scrubs.S01E01.1080p.WEB-DL.DDP5.1.H.264-NTb",
        "guid-noyear-001",
        &yesterday,
        None,
    );

    let targets = vec![nats_jobs::MatchTarget {
        id: "target-1".to_string(),
        target: "Scrubs S01E01".to_string(),
        min_score: None,
        semantic_target: None,
    }];

    let config_with =
        kobload_api::services::want::ScoringConfig::new(&targets, &[], 0.5, None, None)
            .with_media_want(&media_want);
    let results_with: Vec<_> =
        kobload_api::services::want::score_batch(&[release_with_year], &config_with)
            .into_iter()
            .filter(|r| r.match_score.is_some())
            .collect();

    let config_without =
        kobload_api::services::want::ScoringConfig::new(&targets, &[], 0.5, None, None)
            .with_media_want(&media_want);
    let results_without: Vec<_> =
        kobload_api::services::want::score_batch(&[release_without_year], &config_without)
            .into_iter()
            .filter(|r| r.match_score.is_some())
            .collect();

    assert!(
        !results_with.is_empty(),
        "year-matching release should score"
    );
    assert!(
        !results_without.is_empty(),
        "non-year release should also score"
    );
    assert!(
        results_with[0].total_score > results_without[0].total_score,
        "year-matching release should score higher: with={:?} without={:?}",
        results_with[0].total_score,
        results_without[0].total_score,
    );
}

#[tokio::test]
async fn test_rss_prefers_year_match() {
    let (server, _mock_dl, _mock_idx) = start_mock_server().await;
    let pool = &server.pool;
    seed_download_client(pool).await;
    let source_id = seed_indexer_source(pool).await;
    let media_type_id = get_tv_media_type_id(pool).await;

    let past_date = days_from_now(-5);
    let yesterday = days_from_now(-1);
    let (pattern_id, pattern) = create_tv_episode_pattern(
        pool,
        media_type_id,
        "Scrubs",
        "1",
        "1",
        Some(2026),
        Some(&past_date),
        None,
    )
    .await;

    let source = watch_repo::get_indexer_source_by_id(pool, source_id)
        .await
        .expect("query")
        .expect("source row");

    // Process both releases in a single batch — year-matching one should win
    let release_with_year = make_tv_release(
        "Scrubs.2026.S01E01.1080p.WEB-DL.DDP5.1.H.264-NTb",
        "guid-rss-year-001",
        &yesterday,
        Some(source_id),
    );
    let release_without_year = make_tv_release(
        "Scrubs.S01E01.1080p.WEB-DL.DDP5.1.H.264-NTb",
        "guid-rss-noyear-001",
        &yesterday,
        Some(source_id),
    );

    // Process both in a single batch — batch selection uses is_better_than()
    process_releases(
        &server.app_state,
        &source,
        &[release_without_year, release_with_year],
        &[pattern],
    )
    .await;

    let matches = watch_repo::list_matches_by_pattern(pool, pattern_id, 10)
        .await
        .expect("list matches");
    // Should have at least one match, and it should be the year-matching release
    assert!(!matches.is_empty(), "should have at least one match");
    assert_eq!(
        matches[0].release_title, "Scrubs.2026.S01E01.1080p.WEB-DL.DDP5.1.H.264-NTb",
        "year-matching release should be the winner"
    );

    server.cleanup().await;
}

// =============================================================================
// Year mismatch rejection tests
// =============================================================================

#[tokio::test]
async fn test_rejects_wrong_year_in_release() {
    // "Scrubs.2001.S01E01" should be rejected for Scrubs (year=2026) want.
    // Per scene rules, year in title = show premiere year for disambiguation.
    let (server, _mock_dl, _mock_idx) = start_mock_server().await;
    let pool = &server.pool;
    seed_download_client(pool).await;
    let source_id = seed_indexer_source(pool).await;
    let media_type_id = get_tv_media_type_id(pool).await;

    let past_date = days_from_now(-5);
    let (pattern_id, pattern) = create_tv_episode_pattern(
        pool,
        media_type_id,
        "Scrubs",
        "1",
        "1",
        Some(2026),
        Some(&past_date),
        None,
    )
    .await;

    let source = watch_repo::get_indexer_source_by_id(pool, source_id)
        .await
        .expect("query")
        .expect("source row");

    let release = make_tv_release(
        "Scrubs.2001.S01E01.1080p.WEB-DL.DDP5.1.H.264-NTb",
        "guid-wrong-year-001",
        &days_from_now(-1),
        Some(source_id),
    );

    process_release(&server.app_state, &source, &release, &[pattern]).await;

    let matches = watch_repo::list_matches_by_pattern(pool, pattern_id, 10)
        .await
        .expect("list matches");
    assert!(
        matches.is_empty(),
        "release with wrong year (2001 vs want 2026) should be rejected"
    );

    server.cleanup().await;
}

#[tokio::test]
async fn test_rejects_revival_for_original_want() {
    // "Scrubs.2026.S01E01" should be rejected for Scrubs (year=2001) want.
    let (server, _mock_dl, _mock_idx) = start_mock_server().await;
    let pool = &server.pool;
    seed_download_client(pool).await;
    let source_id = seed_indexer_source(pool).await;
    let media_type_id = get_tv_media_type_id(pool).await;

    let (pattern_id, pattern) = create_tv_episode_pattern(
        pool,
        media_type_id,
        "Scrubs",
        "1",
        "1",
        Some(2001),
        Some("2001-10-02"),
        None,
    )
    .await;

    let source = watch_repo::get_indexer_source_by_id(pool, source_id)
        .await
        .expect("query")
        .expect("source row");

    let release = make_tv_release(
        "Scrubs.2026.S01E01.1080p.WEB-DL.DDP5.1.H.264-NTb",
        "guid-revival-wrong-001",
        &days_from_now(-1),
        Some(source_id),
    );

    process_release(&server.app_state, &source, &release, &[pattern]).await;

    let matches = watch_repo::list_matches_by_pattern(pool, pattern_id, 10)
        .await
        .expect("list matches");
    assert!(
        matches.is_empty(),
        "revival release (2026) should be rejected for 2001 want"
    );

    server.cleanup().await;
}

#[tokio::test]
async fn test_old_publish_date_rejects_wrong_era_release() {
    // Old DVDRip published in 2005 should be rejected for a 2026 want.
    let (server, _mock_dl, _mock_idx) = start_mock_server().await;
    let pool = &server.pool;
    seed_download_client(pool).await;
    let source_id = seed_indexer_source(pool).await;
    let media_type_id = get_tv_media_type_id(pool).await;

    let past_date = days_from_now(-5);
    let (pattern_id, pattern) = create_tv_episode_pattern(
        pool,
        media_type_id,
        "Scrubs",
        "1",
        "1",
        Some(2026),
        Some(&past_date),
        None,
    )
    .await;

    let source = watch_repo::get_indexer_source_by_id(pool, source_id)
        .await
        .expect("query")
        .expect("source row");

    // Original Scrubs DVDRip published 2005 — rejected by publish_date sanity
    let release = make_tv_release(
        "Scrubs.2001.S01E01.DVDRip.XviD-FoV",
        "guid-old-era-001",
        "2005-06-15",
        Some(source_id),
    );

    process_release(&server.app_state, &source, &release, &[pattern]).await;

    let matches = watch_repo::list_matches_by_pattern(pool, pattern_id, 10)
        .await
        .expect("list matches");
    assert!(
        matches.is_empty(),
        "old DVDRip from 2005 should be rejected by publish_date sanity"
    );

    server.cleanup().await;
}

#[tokio::test]
async fn test_accepts_no_year_in_release() {
    let (server, _mock_dl, _mock_idx) = start_mock_server().await;
    let pool = &server.pool;
    seed_download_client(pool).await;
    let source_id = seed_indexer_source(pool).await;
    let media_type_id = get_tv_media_type_id(pool).await;

    let past_date = days_from_now(-5);
    // Want Scrubs 2026
    let (pattern_id, pattern) = create_tv_episode_pattern(
        pool,
        media_type_id,
        "Scrubs",
        "1",
        "1",
        Some(2026),
        Some(&past_date),
        None,
    )
    .await;

    let source = watch_repo::get_indexer_source_by_id(pool, source_id)
        .await
        .expect("query")
        .expect("source row");

    // Release with no year — can't reject, should pass through
    let release = make_tv_release(
        "Scrubs.S01E01.1080p.WEB-DL.DDP5.1.H.264-NTb",
        "guid-noyear-accept-001",
        &days_from_now(-1),
        Some(source_id),
    );

    process_release(&server.app_state, &source, &release, &[pattern]).await;

    let matches = watch_repo::list_matches_by_pattern(pool, pattern_id, 10)
        .await
        .expect("list matches");
    assert_eq!(
        matches.len(),
        1,
        "release with no year should be accepted (no year to mismatch)"
    );

    server.cleanup().await;
}

// =============================================================================
// Episode title boost test
// =============================================================================

#[tokio::test]
async fn test_episode_title_boosts_score() {
    let media_want = nats_jobs::MediaWant::TvEpisode {
        show_name: "Scrubs".to_string(),
        season: "1".to_string(),
        episode: "1".to_string(),
        year: Some(2026),
        air_date: Some(days_from_now(-5)),
        identifiers: None,
        alternate_titles: None,
        episode_title: Some("My First Day".to_string()),
        quality_profile_id: None,
        quality_overrides: None,
    };

    let yesterday = days_from_now(-1);

    let release_with_title = make_tv_release(
        "Scrubs.2026.S01E01.My.First.Day.720p.WEB-DL",
        "guid-eptitle-001",
        &yesterday,
        None,
    );
    let release_without_title = make_tv_release(
        "Scrubs.2026.S01E01.720p.WEB-DL",
        "guid-noeptitle-001",
        &yesterday,
        None,
    );

    let targets = vec![nats_jobs::MatchTarget {
        id: "target-1".to_string(),
        target: "Scrubs S01E01".to_string(),
        min_score: None,
        semantic_target: None,
    }];

    let config_with =
        kobload_api::services::want::ScoringConfig::new(&targets, &[], 0.5, None, None)
            .with_media_want(&media_want);
    let results_with: Vec<_> =
        kobload_api::services::want::score_batch(&[release_with_title], &config_with)
            .into_iter()
            .filter(|r| r.match_score.is_some())
            .collect();

    let config_without =
        kobload_api::services::want::ScoringConfig::new(&targets, &[], 0.5, None, None)
            .with_media_want(&media_want);
    let results_without: Vec<_> =
        kobload_api::services::want::score_batch(&[release_without_title], &config_without)
            .into_iter()
            .filter(|r| r.match_score.is_some())
            .collect();

    assert!(
        !results_with.is_empty(),
        "release with episode title should score"
    );
    assert!(
        !results_without.is_empty(),
        "release without episode title should also score"
    );
    assert!(
        results_with[0].total_score > results_without[0].total_score,
        "release containing episode title tokens should score higher: with={:?} without={:?}",
        results_with[0].total_score,
        results_without[0].total_score,
    );
}

#[tokio::test]
async fn test_season_episode_title_boosts_score() {
    let yesterday = days_from_now(-1);
    let aired = days_from_now(-5);

    let media_want = nats_jobs::MediaWant::TvSeason {
        show_name: "Scrubs".to_string(),
        season: "1".to_string(),
        year: Some(2026),
        episodes_needed: Some(vec![nats_jobs::EpisodeRef {
            season: "1".to_string(),
            episode: "1".to_string(),
            sort_order: Some(1),
            air_date: Some(aired.clone()),
            in_library: Some(false),
            library_video_height: None,
            title: Some("My First Day".to_string()),
        }]),
        total_episodes: Some(10),
        pack_preference: "auto".to_string(),
        last_air_date: None,
        identifiers: None,
        alternate_titles: None,
        quality_profile_id: None,
        quality_overrides: None,
    };

    let release_with_title = make_tv_release(
        "Scrubs.2026.S01E01.My.First.Day.720p.WEB-DL",
        "guid-season-eptitle-001",
        &yesterday,
        None,
    );
    let release_without_title = make_tv_release(
        "Scrubs.2026.S01E01.720p.WEB-DL",
        "guid-season-noeptitle-001",
        &yesterday,
        None,
    );

    let targets = vec![nats_jobs::MatchTarget {
        id: "target-1".to_string(),
        target: "Scrubs S01".to_string(),
        min_score: None,
        semantic_target: None,
    }];

    let config_with =
        kobload_api::services::want::ScoringConfig::new(&targets, &[], 0.5, None, None)
            .with_media_want(&media_want);
    let results_with: Vec<_> =
        kobload_api::services::want::score_batch(&[release_with_title], &config_with)
            .into_iter()
            .filter(|r| r.match_score.is_some())
            .collect();

    let config_without =
        kobload_api::services::want::ScoringConfig::new(&targets, &[], 0.5, None, None)
            .with_media_want(&media_want);
    let results_without: Vec<_> =
        kobload_api::services::want::score_batch(&[release_without_title], &config_without)
            .into_iter()
            .filter(|r| r.match_score.is_some())
            .collect();

    assert!(
        !results_with.is_empty(),
        "release with episode title should score"
    );
    assert!(
        !results_without.is_empty(),
        "release without episode title should also score"
    );
    assert!(
        results_with[0].total_score > results_without[0].total_score,
        "season: release containing episode title tokens should score higher: with={:?} without={:?}",
        results_with[0].total_score,
        results_without[0].total_score,
    );
}

// =============================================================================
// Season-level tests
// =============================================================================

#[tokio::test]
async fn test_season_rejects_unaired_episode_from_season_want() {
    let (server, _mock_dl, _mock_idx) = start_mock_server().await;
    let pool = &server.pool;
    seed_download_client(pool).await;
    let source_id = seed_indexer_source(pool).await;
    let media_type_id = get_tv_media_type_id(pool).await;

    let past_date = days_from_now(-5);
    let future_date = days_from_now(30);

    let episodes = vec![
        make_episode_ref("1", Some(&past_date)),
        make_episode_ref("2", Some(&past_date)),
        make_episode_ref("6", Some(&future_date)), // unaired
    ];

    let (pattern_id, pattern) = create_tv_season_pattern(
        pool,
        media_type_id,
        "Scrubs",
        "1",
        Some(2026),
        episodes,
        Some(&future_date),
    )
    .await;

    let source = watch_repo::get_indexer_source_by_id(pool, source_id)
        .await
        .expect("query")
        .expect("source row");

    // Release for unaired E06 — should be rejected
    let release = make_tv_release(
        "Scrubs.S01E06.DVDRip.XviD-FoV",
        "guid-season-unaired-001",
        "2022-01-01",
        Some(source_id),
    );

    process_release(&server.app_state, &source, &release, &[pattern]).await;

    let matches = watch_repo::list_matches_by_pattern(pool, pattern_id, 10)
        .await
        .expect("list matches");
    assert!(
        matches.is_empty(),
        "unaired episode within season want should be rejected"
    );

    server.cleanup().await;
}

#[tokio::test]
async fn test_season_accepts_aired_episode_from_season_want() {
    let (server, _mock_dl, _mock_idx) = start_mock_server().await;
    let pool = &server.pool;
    seed_download_client(pool).await;
    let source_id = seed_indexer_source(pool).await;
    let media_type_id = get_tv_media_type_id(pool).await;

    let past_date = days_from_now(-5);
    let future_date = days_from_now(30);
    let yesterday = days_from_now(-1);

    let episodes = vec![
        make_episode_ref("1", Some(&past_date)),
        make_episode_ref("2", Some(&past_date)),
        make_episode_ref("6", Some(&future_date)),
    ];

    let (pattern_id, pattern) = create_tv_season_pattern(
        pool,
        media_type_id,
        "Scrubs",
        "1",
        Some(2026),
        episodes,
        Some(&future_date),
    )
    .await;

    let source = watch_repo::get_indexer_source_by_id(pool, source_id)
        .await
        .expect("query")
        .expect("source row");

    // Release for aired E01 — should be accepted
    let release = make_tv_release(
        "Scrubs.2026.S01E01.1080p.WEB-DL.DDP5.1.H.264-NTb",
        "guid-season-aired-001",
        &yesterday,
        Some(source_id),
    );

    process_release(&server.app_state, &source, &release, &[pattern]).await;

    let matches = watch_repo::list_matches_by_pattern(pool, pattern_id, 10)
        .await
        .expect("list matches");
    assert_eq!(
        matches.len(),
        1,
        "aired episode within season want should match"
    );

    server.cleanup().await;
}

// =============================================================================
// Full Scrubs scenario (end-to-end)
// =============================================================================

#[tokio::test]
async fn test_scrubs_2026_full_scenario() {
    let (server, _mock_dl, _mock_idx) = start_mock_server().await;
    let pool = &server.pool;
    seed_download_client(pool).await;
    let source_id = seed_indexer_source(pool).await;
    let media_type_id = get_tv_media_type_id(pool).await;

    let past_date = days_from_now(-10);
    let future_date = days_from_now(30);
    let yesterday = days_from_now(-1);

    // E01-E05 aired, E06-E10 unaired
    let episodes: Vec<nats_jobs::EpisodeRef> = (1..=10)
        .map(|i| {
            let air = if i <= 5 { &past_date } else { &future_date };
            make_episode_ref(&i.to_string(), Some(air))
        })
        .collect();

    let (pattern_id, pattern) = create_tv_season_pattern(
        pool,
        media_type_id,
        "Scrubs",
        "1",
        Some(2026),
        episodes,
        Some(&future_date),
    )
    .await;

    let source = watch_repo::get_indexer_source_by_id(pool, source_id)
        .await
        .expect("query")
        .expect("source row");

    // Simulate RSS feed with mix of revival and original releases
    let releases = vec![
        // Correct revival releases for aired episodes
        make_tv_release(
            "Scrubs.2026.S01E01.1080p.WEB-DL.DDP5.1.H.264-NTb",
            "guid-full-001",
            &yesterday,
            Some(source_id),
        ),
        make_tv_release(
            "Scrubs.2026.S01E02.1080p.WEB-DL.DDP5.1.H.264-NTb",
            "guid-full-002",
            &yesterday,
            Some(source_id),
        ),
        make_tv_release(
            "Scrubs.2026.S01E03.1080p.WEB-DL.DDP5.1.H.264-NTb",
            "guid-full-003",
            &yesterday,
            Some(source_id),
        ),
        make_tv_release(
            "Scrubs.2026.S01E04.1080p.WEB-DL.DDP5.1.H.264-NTb",
            "guid-full-004",
            &yesterday,
            Some(source_id),
        ),
        make_tv_release(
            "Scrubs.2026.S01E05.1080p.WEB-DL.DDP5.1.H.264-NTb",
            "guid-full-005",
            &yesterday,
            Some(source_id),
        ),
        // Original Scrubs DVDRips for E06-E10 (published years ago, episodes unaired for 2026)
        make_tv_release(
            "Scrubs.S01E06.DVDRip.XviD-FoV",
            "guid-full-006",
            "2005-06-01",
            Some(source_id),
        ),
        make_tv_release(
            "Scrubs.S01E07.DVDRip.XviD-FoV",
            "guid-full-007",
            "2005-06-01",
            Some(source_id),
        ),
        make_tv_release(
            "Scrubs.S01E08.DVDRip.XviD-FoV",
            "guid-full-008",
            "2005-06-01",
            Some(source_id),
        ),
        make_tv_release(
            "Scrubs.S01E09.DVDRip.XviD-FoV",
            "guid-full-009",
            "2005-06-01",
            Some(source_id),
        ),
        make_tv_release(
            "Scrubs.S01E10.DVDRip.XviD-FoV",
            "guid-full-010",
            "2005-06-01",
            Some(source_id),
        ),
    ];

    for release in &releases {
        process_release(
            &server.app_state,
            &source,
            release,
            std::slice::from_ref(&pattern),
        )
        .await;
    }

    let matches = watch_repo::list_matches_by_pattern(pool, pattern_id, 20)
        .await
        .expect("list matches");

    // Should have matches ONLY for aired episodes (E01-E05)
    let matched_guids: Vec<&str> = matches.iter().map(|m| m.release_guid.as_str()).collect();
    for i in 1..=5 {
        let expected = format!("guid-full-{i:03}");
        assert!(
            matched_guids.contains(&expected.as_str()),
            "aired episode E{i:02} should be matched, but wasn't. Matched: {matched_guids:?}"
        );
    }
    for i in 6..=10 {
        let bad = format!("guid-full-{i:03}");
        assert!(
            !matched_guids.contains(&bad.as_str()),
            "unaired episode E{i:02} should NOT be matched, but was"
        );
    }

    server.cleanup().await;
}

// =============================================================================
// Production bug reproduction: Scrubs E08-E10 with RFC 2822 dates + missing air_dates
// =============================================================================

#[tokio::test]
async fn test_production_scrubs_e08_rfc2822_no_airdate() {
    // Exact reproduction of production bug: backlog search grabbed original Scrubs
    // S01E08-E10 (DEADBADUGLY upscales from Jan 2024) for Scrubs 2026 want.
    // Two bugs: (1) RFC 2822 publish dates not parsed, (2) episodes without
    // air_dates after a future-dated episode passed through filters.
    let (server, _mock_dl, _mock_idx) = start_mock_server().await;
    let pool = &server.pool;
    seed_download_client(pool).await;
    let source_id = seed_indexer_source(pool).await;
    let media_type_id = get_tv_media_type_id(pool).await;

    // E01-E07 have air_dates (recent past + future), E08-E10 don't.
    // E07 is in the future so E08-E10 inherit that and should be rejected.
    let e07_air = days_from_now(5);
    let episodes = vec![
        make_episode_ref("1", Some(&days_from_now(-35))),
        make_episode_ref("2", Some(&days_from_now(-35))),
        make_episode_ref("3", Some(&days_from_now(-28))),
        make_episode_ref("4", Some(&days_from_now(-21))),
        make_episode_ref("5", Some(&days_from_now(-14))),
        make_episode_ref("6", Some(&days_from_now(-7))),
        make_episode_ref("7", Some(&e07_air)),
        make_episode_ref("8", None),  // no air_date
        make_episode_ref("9", None),  // no air_date
        make_episode_ref("10", None), // no air_date
    ];

    let (pattern_id, pattern) = create_tv_season_pattern(
        pool,
        media_type_id,
        "Scrubs",
        "1",
        Some(2026),
        episodes,
        Some(&e07_air),
    )
    .await;

    let source = watch_repo::get_indexer_source_by_id(pool, source_id)
        .await
        .expect("query")
        .expect("source row");

    // Original Scrubs DEADBADUGLY upscale with RFC 2822 publish date from Jan 2024
    let release = make_tv_release(
        "Scrubs.S01E08.My.Fifteen.Minutes.Upscale.Hybrid.1080p.WEBRip.DD2.0.H.264-DEADBADUGLY",
        "guid-prod-e08",
        "Sun, 21 Jan 2024 09:23:07 +0000", // RFC 2822 format!
        Some(source_id),
    );

    process_release(&server.app_state, &source, &release, &[pattern]).await;

    let matches = watch_repo::list_matches_by_pattern(pool, pattern_id, 10)
        .await
        .expect("list matches");
    assert!(
        matches.is_empty(),
        "Original Scrubs E08 from 2024 should be rejected for Scrubs 2026 want (RFC 2822 publish date, no air_date after future E07)"
    );

    server.cleanup().await;
}

#[tokio::test]
async fn test_production_scrubs_e10_no_airdate_after_future() {
    // E10 has no air_date. E07 has air_date in the future (+5 days).
    // E10 should be rejected even though it has no air_date — it can't air before E07.
    let (server, _mock_dl, _mock_idx) = start_mock_server().await;
    let pool = &server.pool;
    seed_download_client(pool).await;
    let source_id = seed_indexer_source(pool).await;
    let media_type_id = get_tv_media_type_id(pool).await;

    // Use relative dates so the test doesn't rot as time passes
    let e07_air = days_from_now(5);
    let episodes = vec![
        make_episode_ref("1", Some(&days_from_now(-35))),
        make_episode_ref("2", Some(&days_from_now(-35))),
        make_episode_ref("3", Some(&days_from_now(-28))),
        make_episode_ref("4", Some(&days_from_now(-21))),
        make_episode_ref("5", Some(&days_from_now(-14))),
        make_episode_ref("6", Some(&days_from_now(-7))),
        make_episode_ref("7", Some(&e07_air)),
        make_episode_ref("8", None),
        make_episode_ref("9", None),
        make_episode_ref("10", None),
    ];

    let (pattern_id, pattern) = create_tv_season_pattern(
        pool,
        media_type_id,
        "Scrubs",
        "1",
        Some(2026),
        episodes,
        Some(&e07_air),
    )
    .await;

    let source = watch_repo::get_indexer_source_by_id(pool, source_id)
        .await
        .expect("query")
        .expect("source row");

    // Recent re-upload of original Scrubs E10 — publish date is recent but episode has no air_date
    let yesterday = days_from_now(-1);
    let release = make_tv_release(
        "Scrubs.S01E10.My.Nickname.1080p.WEB-DL.DDP5.1.H.264-NTb",
        "guid-prod-e10",
        &yesterday,
        Some(source_id),
    );

    process_release(&server.app_state, &source, &release, &[pattern]).await;

    let matches = watch_repo::list_matches_by_pattern(pool, pattern_id, 10)
        .await
        .expect("list matches");
    assert!(
        matches.is_empty(),
        "E10 (no air_date, after future E07) should be rejected even with recent publish date"
    );

    server.cleanup().await;
}

// =============================================================================
// Normal show regression test
// =============================================================================

#[tokio::test]
async fn test_normal_show_unaffected() {
    let (server, _mock_dl, _mock_idx) = start_mock_server().await;
    let pool = &server.pool;
    seed_download_client(pool).await;
    let source_id = seed_indexer_source(pool).await;
    let media_type_id = get_tv_media_type_id(pool).await;

    // Breaking Bad S01E01 aired 2008-01-20, show year 2008
    let (pattern_id, pattern) = create_tv_episode_pattern(
        pool,
        media_type_id,
        "Breaking Bad",
        "1",
        "1",
        Some(2008),
        Some("2008-01-20"),
        None,
    )
    .await;

    let source = watch_repo::get_indexer_source_by_id(pool, source_id)
        .await
        .expect("query")
        .expect("source row");

    // Normal show, recent repack — should work fine
    let release = make_tv_release(
        "Breaking.Bad.S01E01.720p.BluRay.x264-DEMAND",
        "guid-normal-001",
        "2024-03-15",
        Some(source_id),
    );

    process_release(&server.app_state, &source, &release, &[pattern]).await;

    let matches = watch_repo::list_matches_by_pattern(pool, pattern_id, 10)
        .await
        .expect("list matches");
    assert_eq!(
        matches.len(),
        1,
        "normal show should not be affected by disambiguation logic"
    );

    server.cleanup().await;
}
