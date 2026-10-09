//! Request-aware file scoring for completed downloads.
//!
//! Scores files based on how well they match the download's metadata
//! (title, author, format preferences) rather than just picking the largest file.

use std::collections::HashMap;
use std::path::Path;

use kob_normalize::canonical;

use crate::download_clients::ScannedFile;
use crate::format;

/// A scored file with metadata about why it scored that way.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ScoredFile {
    pub path: String,
    pub size: i64,
    pub extension: String,
    pub mime_type: Option<String>,
    pub score: i32,
    pub group_key: String,
}

/// Context about what was requested, used to score files.
#[derive(Debug, Default)]
pub struct ScoringContext {
    /// Title of the requested content
    pub title: Option<String>,
    /// Accepted format extensions (e.g., ["epub", "pdf"])
    pub accepted_formats: Vec<String>,
    /// Media kind for format auto-detection ("book", "tv", "movie")
    pub media_kind: Option<String>,
}

/// Known video file extensions.
const VIDEO_EXTENSIONS: &[&str] = &["mkv", "mp4", "avi", "wmv", "webm", "m4v", "flv", "mov"];

/// Known junk file patterns that should score very low.
const JUNK_PATTERNS: &[&str] = &[
    "sample",
    "bonus",
    "extras",
    "nfo",
    ".nfo",
    ".txt",
    ".jpg",
    ".jpeg",
    ".png",
    ".gif",
    ".nzb",
    ".srr",
    "featurette",
    "trailer",
    ".sfv",
    ".par2",
    "behind.the.scenes",
    "deleted.scenes",
    "interview",
];

/// Subtitle file extensions (low but not junk).
const SUBTITLE_EXTENSIONS: &[&str] = &["srt", "sub", "idx", "ass", "ssa", "vtt"];

/// Archive extensions (deprioritize when extracted contents exist).
const ARCHIVE_EXTENSIONS: &[&str] = &["zip", "rar", "7z", "tar", "gz", "bz2", "xz"];

/// Score and rank files from a completed download.
///
/// Returns files sorted by score (highest first).
pub fn score_files(files: &[ScannedFile], ctx: &ScoringContext) -> Vec<ScoredFile> {
    let has_non_archive = files
        .iter()
        .any(|f| !ARCHIVE_EXTENSIONS.contains(&f.extension.as_str()));

    // Auto-accept video formats when media_kind is tv/movie and no explicit formats set
    let effective_formats = if ctx.accepted_formats.is_empty()
        && ctx
            .media_kind
            .as_deref()
            .is_some_and(|mk| mk == "tv" || mk == "movie")
    {
        VIDEO_EXTENSIONS.iter().map(|s| s.to_string()).collect()
    } else {
        ctx.accepted_formats.clone()
    };

    // Size tiebreaker cap based on media kind
    let size_cap: i64 = match ctx.media_kind.as_deref() {
        Some("tv") | Some("movie") => 50_000_000_000, // 50 GB
        _ => 100_000_000,                             // 100 MB
    };

    let mut scored: Vec<ScoredFile> = files
        .iter()
        .map(|f| {
            let mut score: i32 = 0;
            let lower_path = f.relative_path.to_lowercase();
            let filename = Path::new(&f.relative_path)
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_lowercase();

            // Format matching
            if !effective_formats.is_empty() && effective_formats.contains(&f.extension) {
                score += 1000;
            }

            // Subtitle files: low but not junk
            if SUBTITLE_EXTENSIONS.contains(&f.extension.as_str()) {
                score += 50;
            }

            // MIME type matching via extension
            let mime = format::mime_from_extension(&f.extension).map(|s| s.to_string());

            // Title fuzzy match (simple substring check)
            if let Some(ref title) = ctx.title {
                let title_norm = canonical(title);
                let title_words: Vec<&str> = title_norm.split_whitespace().collect();
                let filename_norm = canonical(&filename);
                let matching = title_words
                    .iter()
                    .filter(|w| filename_norm.contains(*w))
                    .count();
                if !title_words.is_empty() {
                    let ratio = matching as f64 / title_words.len() as f64;
                    score += (ratio * 500.0) as i32;
                }
            }

            // Junk file penalty
            for pattern in JUNK_PATTERNS {
                if lower_path.contains(pattern) {
                    score -= 1000;
                    break;
                }
            }

            // Archive penalty when extracted files exist
            if has_non_archive && ARCHIVE_EXTENSIONS.contains(&f.extension.as_str()) {
                score -= 500;
            }

            // Size as tiebreaker (larger = slightly better, capped by media kind)
            score += (f.size.min(size_cap) / 1_000_000) as i32;

            let group_key = normalize_for_grouping(&filename);

            ScoredFile {
                path: f.relative_path.clone(),
                size: f.size,
                extension: f.extension.clone(),
                mime_type: mime,
                score,
                group_key,
            }
        })
        .collect();

    scored.sort_by_key(|f| std::cmp::Reverse(f.score));
    scored
}

/// Group scored files by normalized name (same content, different formats).
pub fn group_files(files: &[ScoredFile]) -> Vec<FileGroup> {
    let mut groups: HashMap<String, Vec<&ScoredFile>> = HashMap::new();

    for file in files {
        if file.group_key.is_empty() {
            continue;
        }
        groups.entry(file.group_key.clone()).or_default().push(file);
    }

    groups
        .into_iter()
        .filter(|(_, files)| files.len() > 1) // Only groups with multiple formats
        .map(|(key, files)| FileGroup {
            name: key,
            formats: files.iter().map(|f| f.extension.clone()).collect(),
            best_path: files.first().map(|f| f.path.clone()).unwrap_or_default(),
        })
        .collect()
}

/// Normalized filename for grouping (strip extension, common suffixes).
pub fn normalize_for_grouping(filename: &str) -> String {
    let lower = filename.to_lowercase();
    // Strip common suffixes like "(retail)", "[retail]", etc.
    let cleaned = lower
        .replace(['(', ')', '[', ']', '{', '}'], " ")
        .split_whitespace()
        .filter(|w| !["retail", "fixed", "proper", "repack"].contains(w))
        .collect::<Vec<_>>()
        .join(" ");
    cleaned.trim().to_string()
}

/// A group of files representing the same content in different formats.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FileGroup {
    pub name: String,
    pub formats: Vec<String>,
    pub best_path: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_file(path: &str, size: i64, ext: &str) -> ScannedFile {
        ScannedFile {
            id: uuid::Uuid::now_v7(),
            absolute_path: format!("/downloads/{path}"),
            relative_path: path.to_string(),
            size,
            extension: ext.to_string(),
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
    fn test_preferred_format_wins() {
        let files = vec![
            make_file("book.pdf", 5_000_000, "pdf"),
            make_file("book.epub", 2_000_000, "epub"),
        ];
        let ctx = ScoringContext {
            accepted_formats: vec!["epub".to_string()],
            ..Default::default()
        };

        let scored = score_files(&files, &ctx);
        assert_eq!(scored[0].extension, "epub");
    }

    #[test]
    fn test_junk_files_ranked_low() {
        let files = vec![
            make_file("book.epub", 2_000_000, "epub"),
            make_file("sample.txt", 100, "txt"),
            make_file("cover.jpg", 50_000, "jpg"),
        ];
        let ctx = ScoringContext::default();

        let scored = score_files(&files, &ctx);
        assert_eq!(scored[0].extension, "epub");
    }

    #[test]
    fn test_archive_deprioritized_when_extracted() {
        let files = vec![
            make_file("book.epub", 2_000_000, "epub"),
            make_file("archive.zip", 3_000_000, "zip"),
        ];
        let ctx = ScoringContext::default();

        let scored = score_files(&files, &ctx);
        assert_eq!(scored[0].extension, "epub");
    }

    #[test]
    fn test_title_match_boost() {
        let files = vec![
            make_file("random_file.epub", 2_000_000, "epub"),
            make_file("the great gatsby.epub", 2_000_000, "epub"),
        ];
        let ctx = ScoringContext {
            title: Some("The Great Gatsby".to_string()),
            ..Default::default()
        };

        let scored = score_files(&files, &ctx);
        assert!(scored[0].path.contains("great gatsby"));
    }

    #[test]
    fn test_file_grouping() {
        let scored = vec![
            ScoredFile {
                path: "book.epub".into(),
                size: 2_000_000,
                extension: "epub".into(),
                mime_type: Some("application/epub+zip".into()),
                score: 1000,
                group_key: "book".into(),
            },
            ScoredFile {
                path: "book.pdf".into(),
                size: 5_000_000,
                extension: "pdf".into(),
                mime_type: Some("application/pdf".into()),
                score: 500,
                group_key: "book".into(),
            },
        ];

        let groups = group_files(&scored);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].formats.len(), 2);
    }

    #[test]
    fn test_video_auto_accept_for_tv() {
        let files = vec![
            make_file("movie.mkv", 2_000_000_000, "mkv"),
            make_file("info.nfo", 100, "nfo"),
            make_file("sample.mkv", 50_000_000, "mkv"),
        ];
        let ctx = ScoringContext {
            media_kind: Some("movie".to_string()),
            ..Default::default()
        };

        let scored = score_files(&files, &ctx);
        // mkv should get format bonus, nfo should be junk, sample should be junk
        assert_eq!(scored[0].extension, "mkv");
        assert!(scored[0].score > scored[1].score);
    }

    #[test]
    fn test_video_junk_patterns() {
        let files = vec![
            make_file("Movie.2024.1080p.mkv", 4_000_000_000, "mkv"),
            make_file("featurette.mkv", 500_000_000, "mkv"),
            make_file("trailer.mkv", 100_000_000, "mkv"),
            make_file("behind.the.scenes.mkv", 800_000_000, "mkv"),
        ];
        let ctx = ScoringContext {
            media_kind: Some("movie".to_string()),
            ..Default::default()
        };

        let scored = score_files(&files, &ctx);
        assert!(scored[0].path.contains("Movie.2024"));
    }

    #[test]
    fn test_subtitle_files_low_but_not_junk() {
        let files = vec![
            make_file("movie.mkv", 2_000_000_000, "mkv"),
            make_file("movie.srt", 50_000, "srt"),
        ];
        let ctx = ScoringContext {
            media_kind: Some("movie".to_string()),
            ..Default::default()
        };

        let scored = score_files(&files, &ctx);
        // srt should have positive score (50 for subtitle bonus)
        let srt = scored.iter().find(|f| f.extension == "srt").unwrap();
        assert!(
            srt.score > 0,
            "Subtitle should have positive score: {}",
            srt.score
        );
    }

    #[test]
    fn test_size_scaling_by_media_kind() {
        // For books, 100MB cap means a 50GB file still only gets 100 points from size
        let files = vec![make_file("big.epub", 50_000_000_000, "epub")];
        let book_ctx = ScoringContext {
            media_kind: Some("book".to_string()),
            ..Default::default()
        };
        let movie_ctx = ScoringContext {
            media_kind: Some("movie".to_string()),
            ..Default::default()
        };

        let book_scored = score_files(&files, &book_ctx);
        let movie_scored = score_files(&files, &movie_ctx);

        // Movie context allows bigger size tiebreaker
        assert!(movie_scored[0].score > book_scored[0].score);
    }
}
