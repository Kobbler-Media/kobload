//! File classification and metadata extraction.
//!
//! Classifies files within a release by their role:
//! - Main content (movie, episode, book)
//! - Alternate formats (same book in epub/mobi/pdf)
//! - Samples
//! - Subtitles
//! - Extras (behind the scenes, deleted scenes)
//! - Artwork (covers, posters)
//! - Metadata (NFO, SFV, proof)
//!
//! Also extracts technical metadata from video/audio files using mediainfo.

use std::path::Path;

use regex::Regex;
use std::sync::LazyLock;

use crate::download_clients::ScannedFile;

use super::ParsedRelease;

pub use nats_jobs::{
    AudioMetadata, FileClassification, FileMetadata, FileRole, MetadataExtractionStatus,
    SubtitleMetadata, VideoMetadata,
};

// =============================================================================
// Constants
// =============================================================================

/// Sample patterns in filename
static SAMPLE_PATTERNS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)[\-._]sample[\-._]?|^sample[\-._]|[\-._]sample$").unwrap());

/// Trailer patterns
static TRAILER_PATTERNS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)[\-._]trailer[\-._]?|^trailer[\-._]|[\-._]trailer$").unwrap()
});

/// Extras patterns
static EXTRAS_PATTERNS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:behind.the.scenes|deleted.scenes?|featurette|bonus|extras?|interview|making.of|bts)").unwrap()
});

/// Proof patterns
static PROOF_PATTERNS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:^proof|[\-._]proof[\-._]?|proof\.(?:jpg|png|gif))").unwrap()
});

/// Subtitle extensions
pub const SUBTITLE_EXTENSIONS: &[&str] = &["srt", "sub", "idx", "ass", "ssa", "vtt", "sup", "pgs"];

/// Video extensions
pub const VIDEO_EXTENSIONS: &[&str] = &[
    "mkv", "mp4", "avi", "wmv", "webm", "m4v", "flv", "mov", "mpg", "mpeg", "ts", "m2ts", "vob",
];

/// Audio extensions
const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "flac", "m4a", "ogg", "opus", "wav", "wma", "aac", "alac", "ape", "dsf", "dff",
];

/// Ebook extensions
const EBOOK_EXTENSIONS: &[&str] = &[
    "epub", "mobi", "azw", "azw3", "pdf", "cbz", "cbr", "cb7", "djvu", "fb2", "lit", "pdb",
];

/// Image/artwork extensions
const IMAGE_EXTENSIONS: &[&str] = &["jpg", "jpeg", "png", "gif", "webp", "bmp", "tiff"];

/// Metadata extensions
const METADATA_EXTENSIONS: &[&str] = &["nfo", "txt", "sfv", "md5", "par2", "srr"];

/// Archive extensions (usually indicates source, not main content)
const ARCHIVE_EXTENSIONS: &[&str] = &["rar", "zip", "7z", "tar", "gz", "bz2", "xz"];

/// Classify a filename (without needing a full ScannedFile).
/// Returns the FileRole as a string for lightweight use in hash index ingest.
/// Also extracts TV season/episodes from the filename.
///
/// `episodes` follows the same rules as `hash_index::derive_season_episodes_from_media`:
/// pack → None; one or more parsed → Some(list); unparsed → None.
pub fn classify_filename(filename: &str) -> (Option<i16>, Option<Vec<i16>>, &'static str) {
    let basename = filename.rsplit('/').next().unwrap_or(filename);
    let lower = basename.to_lowercase();

    let ext = basename.rsplit('.').next().unwrap_or("").to_lowercase();

    // Extension-based classification
    if SUBTITLE_EXTENSIONS.contains(&ext.as_str()) {
        return (None, None, "subtitle");
    }
    if METADATA_EXTENSIONS.contains(&ext.as_str())
        || IMAGE_EXTENSIONS.contains(&ext.as_str())
        || ARCHIVE_EXTENSIONS.contains(&ext.as_str())
    {
        return (None, None, "other");
    }
    if !VIDEO_EXTENSIONS.contains(&ext.as_str()) && !ext.is_empty() {
        return (None, None, "other");
    }

    // Name-based checks
    if SAMPLE_PATTERNS.is_match(&lower)
        || TRAILER_PATTERNS.is_match(&lower)
        || EXTRAS_PATTERNS.is_match(&lower)
    {
        return (None, None, "sample");
    }

    // It's a video — parse season/episodes
    let parsed = super::parse_release(basename, "tv");
    let (season, episodes) = match &parsed.media {
        super::ParsedMedia::Tv(tv) => {
            let s = tv.season.and_then(|v| i16::try_from(v).ok());
            let eps = if tv.is_pack || tv.episodes.is_empty() {
                None
            } else {
                let list: Vec<i16> = tv
                    .episodes
                    .iter()
                    .filter_map(|v| i16::try_from(*v).ok())
                    .collect();
                if list.is_empty() { None } else { Some(list) }
            };
            (s, eps)
        }
        _ => (None, None),
    };

    (season, episodes, "video")
}

/// Maximum sample size relative to largest file (10%)
const SAMPLE_SIZE_RATIO: f64 = 0.1;

/// Absolute sample size cap (200MB)
const SAMPLE_SIZE_CAP: i64 = 200_000_000;

// =============================================================================
// Classification Functions
// =============================================================================

/// Classify a single file within a release.
///
/// # Arguments
///
/// * `file` - The file to classify
/// * `release_parsed` - Parsed metadata for the release (optional)
/// * `all_files` - All files in the release (for comparison)
/// * `media_kind` - Media type: "tv", "movies", "ebooks", etc.
pub fn classify_file(
    file: &ScannedFile,
    release_parsed: Option<&ParsedRelease>,
    all_files: &[ScannedFile],
    media_kind: &str,
) -> FileClassification {
    let filename = Path::new(&file.relative_path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(&file.relative_path);
    let lower = filename.to_lowercase();
    let ext = file.extension.to_lowercase();

    // Quick checks for obvious types
    if SUBTITLE_EXTENSIONS.contains(&ext.as_str()) {
        return FileClassification {
            role: FileRole::Subtitle,
            parsed: None,
            confidence: 0.95,
            group_key: subtitle_group_key(filename),
        };
    }

    if METADATA_EXTENSIONS.contains(&ext.as_str()) {
        return FileClassification {
            role: FileRole::Metadata,
            parsed: None,
            confidence: 0.95,
            group_key: None,
        };
    }

    if IMAGE_EXTENSIONS.contains(&ext.as_str()) {
        // Check if it's proof
        if PROOF_PATTERNS.is_match(&lower) {
            return FileClassification {
                role: FileRole::Proof,
                parsed: None,
                confidence: 0.9,
                group_key: None,
            };
        }
        return FileClassification {
            role: FileRole::Artwork,
            parsed: None,
            confidence: 0.85,
            group_key: None,
        };
    }

    if ARCHIVE_EXTENSIONS.contains(&ext.as_str()) {
        // Archives are usually metadata-ish in extracted releases
        return FileClassification {
            role: FileRole::Metadata,
            parsed: None,
            confidence: 0.6,
            group_key: None,
        };
    }

    // Name-based checks for videos/main content
    if SAMPLE_PATTERNS.is_match(&lower) {
        return FileClassification {
            role: FileRole::Sample,
            parsed: None,
            confidence: 0.95,
            group_key: None,
        };
    }

    if TRAILER_PATTERNS.is_match(&lower) {
        return FileClassification {
            role: FileRole::Trailer,
            parsed: None,
            confidence: 0.9,
            group_key: None,
        };
    }

    if EXTRAS_PATTERNS.is_match(&lower) {
        return FileClassification {
            role: FileRole::Extras,
            parsed: None,
            confidence: 0.85,
            group_key: None,
        };
    }

    // Size-based sample detection for videos
    if VIDEO_EXTENSIONS.contains(&ext.as_str()) {
        let largest = all_files
            .iter()
            .filter(|f| VIDEO_EXTENSIONS.contains(&f.extension.to_lowercase().as_str()))
            .map(|f| f.size)
            .max()
            .unwrap_or(0);

        if largest > 0 && file.size < SAMPLE_SIZE_CAP {
            let ratio = file.size as f64 / largest as f64;
            if ratio < SAMPLE_SIZE_RATIO {
                return FileClassification {
                    role: FileRole::Sample,
                    parsed: None,
                    confidence: 0.8,
                    group_key: None,
                };
            }
        }

        // It's a video - main content
        let file_parsed = super::parse_release(filename, media_kind);
        return FileClassification {
            role: FileRole::MainContent,
            parsed: Some(file_parsed.media),
            confidence: 0.9,
            group_key: video_group_key(filename, release_parsed),
        };
    }

    // Audio files
    if AUDIO_EXTENSIONS.contains(&ext.as_str()) {
        let file_parsed = super::parse_release(filename, media_kind);
        return FileClassification {
            role: FileRole::MainContent,
            parsed: Some(file_parsed.media),
            confidence: 0.85,
            group_key: audio_group_key(filename),
        };
    }

    // Ebooks - use format grouping
    if EBOOK_EXTENSIONS.contains(&ext.as_str()) {
        let group_key = ebook_group_key(filename, all_files, media_kind);
        let role = determine_ebook_role(&file.relative_path, &group_key, all_files);
        let file_parsed = super::parse_release(filename, media_kind);

        return FileClassification {
            role,
            parsed: Some(file_parsed.media),
            confidence: 0.85,
            group_key: Some(group_key),
        };
    }

    // Unknown
    FileClassification {
        role: FileRole::Unknown,
        parsed: None,
        confidence: 0.3,
        group_key: None,
    }
}

/// Classify all files in a release.
pub fn classify_files(
    files: &[ScannedFile],
    release_parsed: Option<&ParsedRelease>,
    media_kind: &str,
) -> Vec<(ScannedFile, FileClassification)> {
    files
        .iter()
        .map(|f| {
            let classification = classify_file(f, release_parsed, files, media_kind);
            (f.clone(), classification)
        })
        .collect()
}

// =============================================================================
// File Metadata Extraction (mediainfo)
// =============================================================================

/// Check if a path represents an archive-nested file.
///
/// Delegates to `archive::is_archive_nested` which validates that the `!`
/// delimiter is preceded by a recognized archive extension, avoiding false
/// positives from filenames containing `!` (e.g., "Teen Titans Go!").
pub fn is_archive_nested(path: &str) -> bool {
    crate::archive::is_archive_nested(path)
}

/// Extract metadata from a media file using mediainfo.
///
/// Returns a FileMetadata with appropriate status:
/// - `Extracted` - metadata was successfully extracted
/// - `SkippedArchiveNested` - file is inside an archive
/// - `UnsupportedFormat` - file format doesn't support metadata extraction
/// - `ToolNotAvailable` - mediainfo not found
/// - `ExtractionFailed` - mediainfo failed
pub async fn extract_file_metadata(file_path: &str) -> FileMetadata {
    // Check for archive-nested files — but only if the file doesn't actually exist
    // on disk. A file that exists is always directly accessible (filenames like
    // "Teen Titans Go!" contain `!` but are regular files, not archive-nested).
    let file_exists = tokio::fs::metadata(file_path).await.is_ok();
    if !file_exists && is_archive_nested(file_path) {
        return FileMetadata {
            status: Some(MetadataExtractionStatus::SkippedArchiveNested),
            status_reason: Some(
                "File is inside an archive; metadata extraction requires extraction first"
                    .to_string(),
            ),
            ..Default::default()
        };
    }

    let path = Path::new(file_path);

    // Only process video/audio files
    let ext = match path.extension().and_then(|s| s.to_str()) {
        Some(e) => e.to_lowercase(),
        None => {
            return FileMetadata {
                status: Some(MetadataExtractionStatus::UnsupportedFormat),
                status_reason: Some("No file extension".to_string()),
                ..Default::default()
            };
        }
    };

    if !VIDEO_EXTENSIONS.contains(&ext.as_str()) && !AUDIO_EXTENSIONS.contains(&ext.as_str()) {
        return FileMetadata {
            status: Some(MetadataExtractionStatus::UnsupportedFormat),
            status_reason: Some(format!("Extension '{ext}' is not a video/audio format")),
            ..Default::default()
        };
    }

    // Probe file using kob-ffmpeg (replaces mediainfo CLI)
    let probe_path = path.to_owned();
    let probe_result =
        match tokio::task::spawn_blocking(move || kob_ffmpeg::probe::probe(&probe_path)).await {
            Ok(Ok(result)) => result,
            Ok(Err(e)) => {
                tracing::debug!(path = %file_path, error = %e, "probe failed for file");
                return FileMetadata {
                    status: Some(MetadataExtractionStatus::ExtractionFailed),
                    status_reason: Some(format!("probe failed: {e}")),
                    ..Default::default()
                };
            }
            Err(e) => {
                return FileMetadata {
                    status: Some(MetadataExtractionStatus::ExtractionFailed),
                    status_reason: Some(format!("probe task panicked: {e}")),
                    ..Default::default()
                };
            }
        };

    probe_result_to_file_metadata(probe_result)
}

/// Convert a kob-ffmpeg ProbeResult to the shared FileMetadata type.
fn probe_result_to_file_metadata(probe: kob_ffmpeg::ProbeResult) -> FileMetadata {
    let video = probe.video_streams.first().map(|v| VideoMetadata {
        codec: v.codec.clone(),
        width: v.width as u32,
        height: v.height as u32,
        bitrate_kbps: v.bitrate_kbps.map(|b| b as u32),
        framerate: v.framerate,
        bit_depth: v.bit_depth.map(|b| b as u8),
        is_hdr: v.is_hdr,
        hdr_format: v.hdr_format.clone(),
        is_interlaced: v.is_interlaced,
    });

    let audio = probe
        .audio_streams
        .iter()
        .map(|a| AudioMetadata {
            codec: a.codec.clone(),
            channels: a.channels as u8,
            bitrate_kbps: a.bitrate_kbps.map(|b| b as u32),
            sample_rate: a.sample_rate.map(|r| r as u32),
            language: a.language.clone(),
            title: a.title.clone(),
            stream_index: Some(a.index as u32),
        })
        .collect();

    let subtitles = probe
        .subtitle_streams
        .iter()
        .map(|s| SubtitleMetadata {
            codec: s.codec.clone(),
            language: s.language.clone(),
            title: s.title.clone(),
            is_forced: s.is_forced,
            is_default: s.is_default,
            stream_index: Some(s.index as u32),
        })
        .collect();

    FileMetadata {
        status: Some(MetadataExtractionStatus::Extracted),
        status_reason: None,
        video,
        audio,
        subtitles,
        duration_seconds: probe.format.duration_seconds,
        container: Some(probe.format.format_name),
        bitrate_kbps: probe.format.bit_rate_kbps.map(|b| b as u32),
    }
}

// =============================================================================
// Group Key Functions
// =============================================================================

/// Generate group key for subtitle files.
fn subtitle_group_key(filename: &str) -> Option<String> {
    // Subtitles group with their video by base name
    // movie.srt -> movie
    // movie.en.srt -> movie
    // movie.en.forced.srt -> movie

    let stem = Path::new(filename)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(filename);

    // Remove language/forced suffixes
    let parts: Vec<&str> = stem.split('.').collect();
    if parts.len() > 1 {
        // Take just the first part (base name)
        Some(parts[0].to_lowercase())
    } else {
        Some(stem.to_lowercase())
    }
}

/// Generate group key for video files.
fn video_group_key(filename: &str, _release_parsed: Option<&ParsedRelease>) -> Option<String> {
    let stem = Path::new(filename)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(filename);

    // Normalize to create group key
    let key = stem
        .to_lowercase()
        .replace(['.', '_', '-', ' '], "")
        // Remove common suffixes
        .replace("1080p", "")
        .replace("720p", "")
        .replace("2160p", "")
        .replace("4k", "")
        .replace("hd", "");

    Some(key)
}

/// Generate group key for audio files.
fn audio_group_key(filename: &str) -> Option<String> {
    // Group by track number/name
    let stem = Path::new(filename)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(filename);

    Some(stem.to_lowercase())
}

/// Generate group key for ebook files.
fn ebook_group_key(filename: &str, all_files: &[ScannedFile], _media_kind: &str) -> String {
    // Use remove_extension to handle compound extensions like .kepub.epub
    let stem = super::common::remove_extension(filename);

    // Normalize the filename to create group key
    let normalized = stem
        .to_lowercase()
        // Remove format hints
        .replace("(epub)", "")
        .replace("[epub]", "")
        .replace("(mobi)", "")
        .replace("[mobi]", "")
        .replace("(pdf)", "")
        .replace("[pdf]", "")
        .replace("(retail)", "")
        .replace("[retail]", "")
        // Normalize separators
        .replace(['.', '_', '-'], " ")
        // Clean up
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");

    // Check if there are multiple ebooks with similar names
    let ebook_count = all_files
        .iter()
        .filter(|f| EBOOK_EXTENSIONS.contains(&f.extension.to_lowercase().as_str()))
        .count();

    if ebook_count <= 1 {
        // Single ebook - simple key
        normalized
    } else {
        // Multiple ebooks - keep more of the name for differentiation
        normalized
    }
}

/// Determine if an ebook is main content or alternate format.
fn determine_ebook_role(
    relative_path: &str,
    group_key: &str,
    all_files: &[ScannedFile],
) -> FileRole {
    // Find all ebooks in the same group
    let group_files: Vec<_> = all_files
        .iter()
        .filter(|f| {
            EBOOK_EXTENSIONS.contains(&f.extension.to_lowercase().as_str())
                && ebook_group_key_matches(f, group_key)
        })
        .collect();

    if group_files.len() <= 1 {
        return FileRole::MainContent;
    }

    // First file in the group (by path) is main, others are alternates
    let first_in_group = group_files
        .iter()
        .min_by_key(|f| &f.relative_path)
        .map(|f| f.relative_path.as_str());

    if first_in_group == Some(relative_path) {
        FileRole::MainContent
    } else {
        FileRole::AlternateFormat
    }
}

/// Check if a file's group key matches.
fn ebook_group_key_matches(file: &ScannedFile, group_key: &str) -> bool {
    let filename = Path::new(&file.relative_path)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(&file.relative_path);

    let file_key = ebook_group_key(filename, &[], "ebooks");
    file_key == group_key
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use nats_jobs::ParsedMedia;
    use uuid::Uuid;

    fn make_file(path: &str, size: i64) -> ScannedFile {
        let ext = Path::new(path)
            .extension()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();

        ScannedFile {
            id: Uuid::now_v7(),
            absolute_path: format!("/downloads/{path}"),
            relative_path: path.to_string(),
            size,
            extension: ext,
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

    #[test]
    fn test_subtitle_classification() {
        let files = vec![
            make_file("movie.mkv", 1_000_000_000),
            make_file("movie.srt", 50_000),
        ];

        let class = classify_file(&files[1], None, &files, "movies");
        assert_eq!(class.role, FileRole::Subtitle);
    }

    #[test]
    fn test_sample_by_name() {
        let files = vec![
            make_file("movie.mkv", 1_000_000_000),
            make_file("movie-sample.mkv", 50_000_000),
        ];

        let class = classify_file(&files[1], None, &files, "movies");
        assert_eq!(class.role, FileRole::Sample);
    }

    #[test]
    fn test_sample_by_size() {
        let files = vec![
            make_file("movie.mkv", 5_000_000_000),
            make_file("preview.mkv", 50_000_000), // 1% of main file
        ];

        let class = classify_file(&files[1], None, &files, "movies");
        assert_eq!(class.role, FileRole::Sample);
    }

    #[test]
    fn test_extras_classification() {
        let files = vec![
            make_file("movie.mkv", 1_000_000_000),
            make_file("behind.the.scenes.mkv", 500_000_000),
        ];

        let class = classify_file(&files[1], None, &files, "movies");
        assert_eq!(class.role, FileRole::Extras);
    }

    #[test]
    fn test_trailer_classification() {
        let files = vec![
            make_file("movie.mkv", 1_000_000_000),
            make_file("movie-trailer.mkv", 100_000_000),
        ];

        let class = classify_file(&files[1], None, &files, "movies");
        assert_eq!(class.role, FileRole::Trailer);
    }

    #[test]
    fn test_ebook_alternate_format() {
        let files = vec![
            make_file("book.epub", 2_000_000),
            make_file("book.mobi", 2_500_000),
            make_file("book.pdf", 5_000_000),
        ];

        // First ebook should be main, others alternate
        let class1 = classify_file(&files[0], None, &files, "ebooks");
        let class2 = classify_file(&files[1], None, &files, "ebooks");

        // At least one should be main, one should be alternate
        assert!(class1.role == FileRole::MainContent || class2.role == FileRole::MainContent);
        assert!(
            class1.group_key.is_some() && class2.group_key.is_some(),
            "Both should have group keys"
        );
    }

    #[test]
    fn test_nfo_classification() {
        let files = vec![make_file("release.nfo", 1000)];

        let class = classify_file(&files[0], None, &files, "movies");
        assert_eq!(class.role, FileRole::Metadata);
    }

    #[test]
    fn test_artwork_classification() {
        let files = vec![
            make_file("movie.mkv", 1_000_000_000),
            make_file("cover.jpg", 500_000),
        ];

        let class = classify_file(&files[1], None, &files, "movies");
        assert_eq!(class.role, FileRole::Artwork);
    }

    #[test]
    fn test_proof_classification() {
        let files = vec![
            make_file("movie.mkv", 1_000_000_000),
            make_file("proof.jpg", 500_000),
        ];

        let class = classify_file(&files[1], None, &files, "movies");
        assert_eq!(class.role, FileRole::Proof);
    }

    #[test]
    fn test_ebook_group_key_compound_extension() {
        // .kepub.epub should produce the same group key as .epub
        let key1 = ebook_group_key("Book Title.kepub.epub", &[], "ebooks");
        let key2 = ebook_group_key("Book Title.epub", &[], "ebooks");
        assert_eq!(
            key1, key2,
            "Compound extension should produce same group key"
        );
    }

    #[test]
    fn test_ebook_role_different_dirs_same_filename() {
        let files = vec![
            make_file("dir_a/book.epub", 2_000_000),
            make_file("dir_b/book.epub", 2_500_000),
        ];

        let class1 = classify_file(&files[0], None, &files, "ebooks");
        let class2 = classify_file(&files[1], None, &files, "ebooks");

        // They have the same group key but different relative paths,
        // so one should be main and one should be alternate
        assert!(
            (class1.role == FileRole::MainContent && class2.role == FileRole::AlternateFormat)
                || (class1.role == FileRole::AlternateFormat
                    && class2.role == FileRole::MainContent),
            "Expected one MainContent and one AlternateFormat, got {:?} and {:?}",
            class1.role,
            class2.role
        );
    }

    #[test]
    fn test_classify_file_tv_episode_parsed() {
        let files = vec![make_file("S01E05.The.Episode.Title.mkv", 1_000_000_000)];
        let class = classify_file(&files[0], None, &files, "tv");
        assert_eq!(class.role, FileRole::MainContent);
        match &class.parsed {
            Some(ParsedMedia::Tv(tv)) => {
                assert!(
                    tv.episodes.contains(&5),
                    "Expected episode 5, got: {:?}",
                    tv.episodes
                );
            }
            other => panic!("Expected Some(Tv), got: {:?}", other),
        }
    }

    #[test]
    fn test_classify_file_multi_episode_parsed() {
        let files = vec![make_file("S01E01E02.mkv", 2_000_000_000)];
        let class = classify_file(&files[0], None, &files, "tv");
        match &class.parsed {
            Some(ParsedMedia::Tv(tv)) => {
                assert!(
                    tv.episodes.contains(&1) && tv.episodes.contains(&2),
                    "Expected episodes [1,2], got: {:?}",
                    tv.episodes
                );
            }
            other => panic!("Expected Some(Tv), got: {:?}", other),
        }
    }

    #[test]
    fn test_classify_file_ebook_parsed() {
        let files = vec![make_file("Brandon.Sanderson-Mistborn.epub", 500_000)];
        let class = classify_file(&files[0], None, &files, "ebooks");
        assert_eq!(class.role, FileRole::MainContent);
        match &class.parsed {
            Some(ParsedMedia::Book(book)) => {
                assert!(!book.title.is_empty(), "Expected non-empty title");
            }
            other => panic!("Expected Some(Book), got: {:?}", other),
        }
    }

    #[test]
    fn test_classify_file_movie_parsed() {
        let files = vec![make_file("Movie.Title.2024.1080p.mkv", 4_000_000_000)];
        let class = classify_file(&files[0], None, &files, "movies");
        assert_eq!(class.role, FileRole::MainContent);
        match &class.parsed {
            Some(ParsedMedia::Movie(movie)) => {
                assert!(!movie.title.is_empty(), "Expected non-empty title");
            }
            other => panic!("Expected Some(Movie), got: {:?}", other),
        }
    }
}
