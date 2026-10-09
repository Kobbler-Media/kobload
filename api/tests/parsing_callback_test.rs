//! Integration tests for the media parsing system in callbacks.
//!
//! These tests verify that release names are correctly parsed and the
//! interpretation data is properly serialized in callback payloads.

mod common;

use kobload_api::download_clients::ScannedFile;
use kobload_api::parsing::{
    FileClassification, FileRole, ParsedMedia, classify_file, parse_release,
};
use nats_jobs::{DownloadCompletedEvent, DownloadFileInfo};
use uuid::Uuid;

/// Helper to create a ScannedFile for testing.
fn make_scanned_file(absolute_path: &str, size: i64) -> ScannedFile {
    let path = std::path::Path::new(absolute_path);
    let relative_path = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();
    let extension = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();

    ScannedFile {
        id: Uuid::now_v7(),
        absolute_path: absolute_path.to_string(),
        relative_path,
        size,
        extension,
        split_rar_volumes: None,
        distributed_rar_sources: None,
        distributed_numeric_sources: None,
        numeric_split_type: None,
        score: None,
        classification: None,
        file_metadata: None,
        mime_type: None,
    }
}

/// Verify that TV release names are parsed correctly.
#[test]
fn parses_tv_episode_release() {
    let parsed = parse_release(
        "Show.Name.S01E05.Episode.Title.1080p.WEB-DL.x264-GROUP",
        "tv",
    );

    assert_eq!(parsed.title, "Show Name");
    assert!(matches!(parsed.media, ParsedMedia::Tv(_)));

    if let ParsedMedia::Tv(tv) = &parsed.media {
        assert_eq!(tv.season, Some(1));
        assert_eq!(tv.episodes, vec![5]);
        assert!(!tv.is_pack);
    }
}

/// Verify that movie release names are parsed correctly.
#[test]
fn parses_movie_release() {
    let parsed = parse_release(
        "Movie.Title.2024.Directors.Cut.1080p.BluRay.x264-GROUP",
        "movies",
    );

    assert_eq!(parsed.title, "Movie Title");
    assert!(matches!(parsed.media, ParsedMedia::Movie(_)));

    if let ParsedMedia::Movie(movie) = &parsed.media {
        assert_eq!(movie.year, Some(2024));
        assert_eq!(movie.edition.as_deref(), Some("Directors Cut"));
    }
}

/// Verify that book release names are parsed correctly.
#[test]
fn parses_book_release() {
    let parsed = parse_release(
        "Brandon.Sanderson-Mistborn.The.Final.Empire.[EPUB]",
        "ebooks",
    );

    assert!(matches!(parsed.media, ParsedMedia::Book(_)));

    if let ParsedMedia::Book(book) = &parsed.media {
        // The parser may split on dots, so "Brandon.Sanderson" becomes "Brandon Sanderson" or "Brandon"
        // depending on the parsing strategy. We just verify there's an author.
        assert!(book.author.is_some());
        assert!(book.title.contains("Mistborn"));
    }
}

/// Verify that PROPER/REPACK flags are detected.
#[test]
fn detects_release_flags() {
    let proper = parse_release("Show.S01E05.PROPER.1080p.WEB-DL", "tv");
    assert!(proper.flags.is_proper);

    let repack = parse_release("Show.S01E05.REPACK.1080p.WEB-DL", "tv");
    assert!(repack.flags.is_repack);
}

/// Verify that callback payload serializes with release_interpretation.
#[test]
fn callback_payload_includes_release_interpretation() {
    let parsed = parse_release("Show.Name.S01E05.1080p.WEB-DL", "tv");

    let event = DownloadCompletedEvent {
        download_id: Uuid::new_v4(),
        status: "completed".to_string(),
        name: "Show.Name.S01E05.1080p.WEB-DL".to_string(),
        metadata: serde_json::json!({"book_id": "test"}),
        files: vec![],
        file_groups: None,
        release_interpretation: Some(parsed),
        error: None,
        recoverable_error: None,
        idempotency_key: None,
        is_upgrade: false,
        quality_score: None,
        replaces_download_id: None,
        release_info: None,
    };

    let json = serde_json::to_value(&event).unwrap();

    // Verify release_interpretation is present and has expected structure
    assert!(json["release_interpretation"].is_object());
    assert_eq!(json["release_interpretation"]["title"], "Show Name");
    assert!(json["release_interpretation"]["media"].is_object());
    assert_eq!(json["release_interpretation"]["media"]["type"], "tv");
    // Verify name is always present
    assert_eq!(json["name"], "Show.Name.S01E05.1080p.WEB-DL");
}

/// Verify that file classification is correctly serialized.
#[test]
fn file_info_includes_classification() {
    let classification = FileClassification {
        role: FileRole::MainContent,
        parsed: None,
        confidence: 0.95,
        group_key: Some("book".to_string()),
    };

    let file_info = DownloadFileInfo {
        id: Some(Uuid::new_v4()),
        path: "Mistborn.epub".to_string(),
        absolute_path: None,
        url: Some("/api/v1/downloads/test/files/Mistborn.epub".to_string()),
        size: 1_500_000,
        extension: "epub".to_string(),
        mime_type: Some("application/epub+zip".to_string()),
        score: Some(95),
        classification: Some(classification),
        file_metadata: None,
        matched_episode: None,
    };

    let json = serde_json::to_value(&file_info).unwrap();

    // Verify classification is present
    assert!(json["classification"].is_object());
    assert_eq!(json["classification"]["role"], "main_content");
    assert_eq!(json["classification"]["confidence"], 0.95);
    assert_eq!(json["classification"]["group_key"], "book");
}

/// Verify file classification detects samples.
#[test]
fn classifies_sample_files() {
    let sample = make_scanned_file("/downloads/Movie-sample.mkv", 50_000_000); // 50MB
    let main = make_scanned_file("/downloads/Movie.mkv", 5_000_000_000); // 5GB

    let all_files = vec![sample.clone(), main.clone()];

    let sample_class = classify_file(&sample, None, &all_files, "movies");
    assert_eq!(sample_class.role, FileRole::Sample);

    let main_class = classify_file(&main, None, &all_files, "movies");
    assert_eq!(main_class.role, FileRole::MainContent);
}

/// Verify file classification detects subtitle files.
#[test]
fn classifies_subtitle_files() {
    let subtitle = make_scanned_file("/downloads/Movie.en.srt", 50_000);

    let classification = classify_file(&subtitle, None, &[], "movies");
    assert_eq!(classification.role, FileRole::Subtitle);
}

/// Verify file classification assigns group_key for alternate formats.
#[test]
fn groups_alternate_book_formats() {
    let epub = make_scanned_file("/downloads/Mistborn.epub", 1_500_000);
    let mobi = make_scanned_file("/downloads/Mistborn.mobi", 1_800_000);

    let all_files = vec![epub.clone(), mobi.clone()];

    let epub_class = classify_file(&epub, None, &all_files, "ebooks");
    let mobi_class = classify_file(&mobi, None, &all_files, "ebooks");

    // Both should have the same group_key since they're the same book
    assert!(epub_class.group_key.is_some());
    assert!(mobi_class.group_key.is_some());
    assert_eq!(epub_class.group_key, mobi_class.group_key);
}

/// Verify that the full callback flow produces expected JSON structure.
#[test]
fn full_callback_structure_for_kobbler() {
    // Simulate a completed download with files
    let release_name = "Brandon.Sanderson-Mistborn.The.Final.Empire.[EPUB]";
    let parsed = parse_release(release_name, "ebooks");

    let epub_file = make_scanned_file("/downloads/complete/Mistborn.epub", 1_500_000);

    let all_files = vec![epub_file.clone()];
    let classification = classify_file(&epub_file, Some(&parsed), &all_files, "ebooks");

    let file_info = DownloadFileInfo {
        id: Some(Uuid::new_v4()),
        path: "Mistborn.epub".to_string(),
        absolute_path: None,
        url: Some("/api/v1/downloads/test-id/files/Mistborn.epub".to_string()),
        size: 1_500_000,
        extension: "epub".to_string(),
        mime_type: Some("application/epub+zip".to_string()),
        score: Some(95),
        classification: Some(classification),
        file_metadata: None,
        matched_episode: None,
    };

    let event = DownloadCompletedEvent {
        download_id: Uuid::new_v4(),
        status: "completed".to_string(),
        name: "Mistborn.The.Final.Empire-GROUP".to_string(),
        metadata: serde_json::json!({
            "book_id": "01234567-0123-0123-0123-0123456789ab",
            "title": "Mistborn: The Final Empire",
            "author": "Brandon Sanderson"
        }),
        files: vec![file_info],
        file_groups: None,
        release_interpretation: Some(parsed),
        error: None,
        recoverable_error: None,
        idempotency_key: Some(Uuid::new_v4()),
        is_upgrade: false,
        quality_score: None,
        replaces_download_id: None,
        release_info: None,
    };

    let json = serde_json::to_value(&event).unwrap();

    // Verify structure Kobbler expects
    assert!(json["download_id"].is_string());
    assert_eq!(json["status"], "completed");
    assert!(json["metadata"]["book_id"].is_string());

    // Files array with classification
    assert!(json["files"].is_array());
    assert_eq!(json["files"][0]["extension"], "epub");
    assert!(json["files"][0]["classification"].is_object());
    assert_eq!(json["files"][0]["classification"]["role"], "main_content");

    // Release interpretation
    assert!(json["release_interpretation"].is_object());
    assert!(json["release_interpretation"]["media"]["type"] == "book");

    // Idempotency key
    assert!(json["idempotency_key"].is_string());
}

/// Verify TV special detection (Season 0).
#[test]
fn parses_tv_special() {
    let parsed = parse_release("Show.Name.S00E05.Christmas.Special.1080p.WEB-DL", "tv");

    if let ParsedMedia::Tv(tv) = &parsed.media {
        assert_eq!(tv.season, Some(0));
        assert!(tv.is_special);
    } else {
        panic!("Expected TV media type");
    }
}

/// Verify season pack detection.
#[test]
fn parses_season_pack() {
    let parsed = parse_release("Show.Name.S01.1080p.BluRay.x264-GROUP", "tv");

    if let ParsedMedia::Tv(tv) = &parsed.media {
        assert_eq!(tv.season, Some(1));
        assert!(tv.is_pack);
        assert!(tv.episodes.is_empty());
    } else {
        panic!("Expected TV media type");
    }
}

/// Verify quality parsing includes sources.
#[test]
fn parses_quality_sources() {
    let bluray = parse_release("Movie.2024.1080p.BluRay.x264", "movies");
    assert_eq!(bluray.quality.source.as_deref(), Some("bluray"));

    let webdl = parse_release("Show.S01E01.1080p.WEB-DL.x264", "tv");
    assert_eq!(webdl.quality.source.as_deref(), Some("web-dl"));

    let hdtv = parse_release("Show.S01E01.720p.HDTV.x264", "tv");
    assert_eq!(hdtv.quality.source.as_deref(), Some("hdtv"));
}
