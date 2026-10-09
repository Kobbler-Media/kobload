//! Tests for the default quality profile fallback in want processing.

mod common;

use common::TestServer;
use kobload_api::media::quality_media_kind;
use kobload_api::repositories::quality_profiles as quality_repo;
use nats_jobs::MediaWant;

/// Verify that `quality_media_kind()` maps TV variants to "tv", Movie to "movie",
/// and everything else to None.
#[test]
fn test_quality_media_kind_mapping() {
    let tv_episode = MediaWant::TvEpisode {
        show_name: "Test".into(),
        season: "1".into(),
        episode: "1".into(),
        year: None,
        identifiers: None,
        alternate_titles: None,
        episode_title: None,
        quality_profile_id: None,
        quality_overrides: None,
        air_date: None,
    };
    assert_eq!(quality_media_kind(&tv_episode), Some("tv"));

    let tv_season = MediaWant::TvSeason {
        show_name: "Test".into(),
        season: "1".into(),
        year: None,
        episodes_needed: None,
        identifiers: None,
        alternate_titles: None,
        quality_profile_id: None,
        quality_overrides: None,
        last_air_date: None,
        pack_preference: "never".to_string(),
        total_episodes: None,
    };
    assert_eq!(quality_media_kind(&tv_season), Some("tv"));

    let movie = MediaWant::Movie {
        title: "Test".into(),
        year: None,
        identifiers: None,
        alternate_titles: None,
        quality_profile_id: None,
        quality_overrides: None,
        release_date: None,
        in_library: None,
        library_video_height: None,
    };
    assert_eq!(quality_media_kind(&movie), Some("movie"));

    let book = MediaWant::Book {
        title: "Test".into(),
        author: None,
        isbn: None,
        format_preferences: vec![],
        identifiers: None,
        series: None,
        series_position: None,
        release_date: None,
    };
    assert_eq!(quality_media_kind(&book), None);

    let audiobook = MediaWant::Audiobook {
        title: "Test".into(),
        author: None,
        narrator: None,
        isbn: None,
        format_preferences: vec![],
        identifiers: None,
        series: None,
        series_position: None,
        release_date: None,
    };
    assert_eq!(quality_media_kind(&audiobook), None);

    let comic = MediaWant::Comic {
        title: "Test".into(),
        issue: None,
        series: None,
        year: None,
    };
    assert_eq!(quality_media_kind(&comic), None);

    let music = MediaWant::Music {
        artist: "Test".into(),
        album: None,
        year: None,
    };
    assert_eq!(quality_media_kind(&music), None);

    let game = MediaWant::Game {
        title: "Test".into(),
        platform: None,
        year: None,
    };
    assert_eq!(quality_media_kind(&game), None);

    let software = MediaWant::Software {
        name: "Test".into(),
        version: None,
        platform: None,
    };
    assert_eq!(quality_media_kind(&software), None);
}

/// Verify that seeded default quality profiles exist and are returned by
/// `get_default_for_media_kind()`.
#[tokio::test]
async fn test_get_default_for_media_kind() {
    let server = TestServer::start().await;

    // Seed default quality profiles (TestServer doesn't call this automatically)
    kobload_api::startup::ensure_default_quality_profiles(&server.pool)
        .await
        .expect("seed quality profiles");

    // TV default should be "TV - 1080p"
    let tv_profile = quality_repo::get_default_for_media_kind(&server.pool, "tv")
        .await
        .expect("query tv default")
        .expect("tv default should exist");
    assert_eq!(tv_profile.name, "TV - 1080p");
    assert_eq!(tv_profile.media_kind, "tv");
    assert!(tv_profile.is_default);

    // Movie default should be "Movies - 1080p"
    let movie_profile = quality_repo::get_default_for_media_kind(&server.pool, "movie")
        .await
        .expect("query movie default")
        .expect("movie default should exist");
    assert_eq!(movie_profile.name, "Movies - 1080p");
    assert_eq!(movie_profile.media_kind, "movie");
    assert!(movie_profile.is_default);

    // No default for a kind that doesn't exist
    let none_profile = quality_repo::get_default_for_media_kind(&server.pool, "nonexistent")
        .await
        .expect("query nonexistent default");
    assert!(none_profile.is_none());

    server.cleanup().await;
}
