//! Folder structure parsing for media files.
//!
//! Understands hierarchical paths like:
//! - TV: `Show Name/Season 01/S01E05.mkv`
//! - Movies: `Movie Name (2024)/Movie.mkv`
//! - Music: `Artist/Album (2024)/01 - Track.flac`

use regex::Regex;
use std::sync::LazyLock;

#[allow(unused_imports)]
use super::common::{
    extract_folder_metadata, is_generic_audiobook_filename, normalize_spaces,
    parse_dot_separated_dirname, remove_extension, strip_bracket_tags, strip_folder_noise,
    strip_leading_number, strip_part_suffix,
};
use super::types::ParsedRelease;
use super::{ParsedMedia, parse_release};
use serde::{Deserialize, Serialize};

// =============================================================================
// Types
// =============================================================================

/// Result of parsing a full file path.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathParsed {
    /// Metadata extracted from folder structure
    pub folder_context: FolderContext,
    /// Metadata extracted from filename
    pub file_parsed: ParsedRelease,
    /// Combined/resolved metadata (folder + file)
    pub resolved: ParsedRelease,
}

/// Context extracted from folder structure.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FolderContext {
    /// Show/movie/artist name from folder
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Season number from "Season XX" folder
    #[serde(skip_serializing_if = "Option::is_none")]
    pub season: Option<u32>,
    /// Year from folder name
    #[serde(skip_serializing_if = "Option::is_none")]
    pub year: Option<u32>,
    /// Artist from music folder structure
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artist: Option<String>,
    /// Album from music folder structure
    #[serde(skip_serializing_if = "Option::is_none")]
    pub album: Option<String>,
    /// Author from book folder structure
    #[serde(skip_serializing_if = "Option::is_none")]
    pub author: Option<String>,
    /// Series from book folder structure
    #[serde(skip_serializing_if = "Option::is_none")]
    pub series: Option<String>,
    /// Series position from folder structure
    #[serde(skip_serializing_if = "Option::is_none")]
    pub series_position: Option<String>,
    /// Narrator from folder structure (audiobooks)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub narrator: Option<String>,
}

// =============================================================================
// Regexes
// =============================================================================

/// Season folder pattern: Season 01, Season.01, Season 1, S01, S1
static SEASON_FOLDER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(?:Season[\s.]?|S)(?P<num>\d{1,2})$").unwrap());

/// Specials folder pattern
static SPECIALS_FOLDER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^(?:Specials?|Season\.?0|S00|Extras?)$").unwrap());

/// Year in folder name: (2024), [2024]
static YEAR_FOLDER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[\(\[](?P<year>\d{4})[\)\]]").unwrap());

// =============================================================================
// Parsing
// =============================================================================

/// Parse a file path to extract hierarchical metadata.
///
/// # Arguments
///
/// * `path` - Full path like "Show Name/Season 01/S01E05.mkv"
/// * `media_kind` - Media type: "tv", "movies", "music", etc.
///
/// # Returns
///
/// [`PathParsed`] with folder context, file parsing, and combined result.
pub fn parse_path(path: &str, media_kind: &str) -> PathParsed {
    // Split path into components
    let components: Vec<&str> = path.split(['/', '\\']).filter(|s| !s.is_empty()).collect();

    if components.is_empty() {
        return empty_path_result(media_kind);
    }

    // Extract folder context based on media type
    let folder_context = match media_kind {
        "tv" => parse_tv_folder_context(&components),
        "movies" => parse_movie_folder_context(&components),
        "music" => parse_music_folder_context(&components),
        "ebooks" | "audiobooks" => parse_book_folder_context(&components),
        "sports" => parse_sport_folder_context(&components),
        _ => {
            // Auto-detect: try TV folder context (looks for "Season XX" folders),
            // fall back to movie folder context for generic title+year extraction.
            let tv_ctx = parse_tv_folder_context(&components);
            if tv_ctx.season.is_some() {
                tv_ctx
            } else {
                parse_movie_folder_context(&components)
            }
        }
    };

    // Parse the filename
    let filename = components.last().unwrap_or(&"");
    let file_parsed = parse_release(filename, media_kind);

    // Combine folder context with file parsing
    let resolved = resolve_with_context(file_parsed.clone(), &folder_context, media_kind, filename);

    PathParsed {
        folder_context,
        file_parsed,
        resolved,
    }
}

/// Parse folder context for TV shows.
fn parse_tv_folder_context(components: &[&str]) -> FolderContext {
    let mut ctx = FolderContext::default();

    // Work backwards from filename
    // Pattern: Show Name/Season XX/filename.mkv
    // or: Show Name (Year)/Season XX/filename.mkv

    let depth = components.len();

    for (i, component) in components.iter().enumerate().rev() {
        // Skip the filename
        if i == depth - 1 {
            continue;
        }

        // Check for season folder
        if let Some(caps) = SEASON_FOLDER_RE.captures(component) {
            if let Some(num) = caps.name("num") {
                ctx.season = num.as_str().parse().ok();
            }
            continue;
        }

        // Check for specials folder
        if SPECIALS_FOLDER_RE.is_match(component) {
            ctx.season = Some(0);
            continue;
        }

        // First non-season folder is likely the show name
        if ctx.title.is_none() {
            let (title, year) = extract_title_and_year(component);
            ctx.title = Some(title);
            if year.is_some() {
                ctx.year = year;
            }
        }
    }

    ctx
}

/// Parse folder context for movies.
fn parse_movie_folder_context(components: &[&str]) -> FolderContext {
    let mut ctx = FolderContext::default();

    // Movies usually have: Movie Name (Year)/filename.mkv

    if components.len() >= 2 {
        let folder = components[components.len() - 2];
        let (title, year) = extract_title_and_year(folder);
        ctx.title = Some(title);
        ctx.year = year;
    }

    ctx
}

/// Parse folder context for music.
fn parse_music_folder_context(components: &[&str]) -> FolderContext {
    let mut ctx = FolderContext::default();

    // Music usually has: Artist/Album (Year)/track.flac
    // or: Artist - Album (Year)/track.flac

    let depth = components.len();

    if depth >= 2 {
        // Second-to-last is album folder
        let album_folder = components[depth - 2];
        let (album, year) = extract_title_and_year(album_folder);
        ctx.album = Some(album);
        ctx.year = year;

        // Third-to-last is artist folder
        if depth >= 3 {
            ctx.artist = Some(normalize_spaces(components[depth - 3]));
        }
    }

    ctx
}

/// Parse folder context for books/audiobooks.
///
/// Common patterns:
/// - `Author Name/Book Title.epub` → author from parent
/// - `Author Name/Series Name/Book Title.epub` → author from grandparent, series from parent
/// - `Author - Title/01.mp3` → split on " - ", author+title from parent
/// - `JEFF.KINNEY-DIARY.OF.A.WIMPY.KID.SERIES.1.hash-17/file.mp3` → scene release dir
/// - `1992 - Title (Narrator) 64k {120mb}/file.mp3` → strip noise, extract title
fn parse_book_folder_context(components: &[&str]) -> FolderContext {
    let mut ctx = FolderContext::default();
    let depth = components.len();

    if depth >= 3 {
        // Try dot-separated scene release on grandparent
        let grandparent = components[depth - 3];
        if let Some(dot) = parse_dot_separated_dirname(grandparent) {
            ctx.author = dot.author;
            ctx.series = dot.series;
            ctx.title = dot.title;
            if ctx.year.is_none() {
                ctx.year = dot.year;
            }
        } else {
            let meta = extract_folder_metadata(grandparent);
            let gp_clean = strip_bracket_tags(&meta.clean_name);
            let gp_clean = normalize_spaces(&gp_clean);
            // Try "Author - Title/Series" split on grandparent
            if let Some((author, title)) = split_author_title(&gp_clean) {
                ctx.author = Some(author);
                ctx.title = Some(title);
            } else {
                ctx.author = Some(gp_clean);
            }
            if ctx.year.is_none() {
                ctx.year = meta.year;
            }
            if meta.narrator.is_some() {
                ctx.narrator = meta.narrator;
            }
        }

        // Parent dir: try audiobook-specific parsers, then fall back
        let parent_raw = components[depth - 2];
        let parent_meta = extract_folder_metadata(parent_raw);
        let parent_clean = strip_bracket_tags(&parent_meta.clean_name);
        let parent_clean = normalize_spaces(&parent_clean);
        let parent_stripped = strip_leading_number(&parent_clean);

        if let Some(multi) = parse_multi_dash_audiobook_dir(&parent_stripped) {
            // Multi-dash pattern found in parent (e.g., "Author - BC 005 - Title - Narrator")
            // Multi-dash author is more specific than grandparent-derived author
            if multi.author.is_some() {
                ctx.author = multi.author;
            }
            if multi.title.is_some() {
                ctx.title = multi.title;
            }
            if multi.series.is_some() && ctx.series.is_none() {
                ctx.series = multi.series;
            }
            if multi.series_position.is_some() {
                ctx.series_position = multi.series_position;
            }
            if multi.narrator.is_some() && ctx.narrator.is_none() {
                ctx.narrator = multi.narrator;
            }
        } else if let Some((series, index, title)) = split_series_title(&parent_stripped) {
            if ctx.series.is_none() {
                ctx.series = Some(series);
            }
            ctx.series_position = Some(index);
            ctx.title = Some(title);
        } else if let Some((_, title_part)) = split_author_title(&parent_stripped) {
            ctx.title = Some(title_part);
        } else {
            let (series, year) = extract_title_and_year(&parent_stripped);
            if ctx.series.is_none() {
                ctx.series = Some(series);
            }
            if ctx.year.is_none() {
                ctx.year = year;
            }
        }

        // Capture year/narrator from parent metadata if not already set
        if ctx.year.is_none() {
            ctx.year = parent_meta.year;
        }
        if ctx.narrator.is_none() {
            ctx.narrator = parent_meta.narrator;
        }
    } else if depth >= 2 {
        let parent = components[depth - 2];

        // Try dot-separated scene release
        if let Some(dot) = parse_dot_separated_dirname(parent) {
            ctx.author = dot.author;
            ctx.series = dot.series;
            ctx.title = dot.title;
            ctx.year = dot.year;
        } else {
            // Use extract_folder_metadata to capture year/narrator
            let meta = extract_folder_metadata(parent);
            let cleaned = strip_bracket_tags(&meta.clean_name);
            let cleaned = normalize_spaces(&cleaned);

            ctx.year = meta.year;
            ctx.narrator = meta.narrator;

            // Try audiobook-specific parsers on the cleaned name
            if let Some(multi) = parse_multi_dash_audiobook_dir(&cleaned) {
                ctx.author = multi.author;
                ctx.title = multi.title;
                ctx.series = multi.series;
                ctx.series_position = multi.series_position;
                if multi.narrator.is_some() && ctx.narrator.is_none() {
                    ctx.narrator = multi.narrator;
                }
            } else if let Some((series, index, title)) = split_series_title(&cleaned) {
                ctx.series = Some(series);
                ctx.series_position = Some(index);
                ctx.title = Some(title);
            } else if let Some((author, title)) = split_author_title(&cleaned) {
                // "Author - Title" split
                ctx.author = Some(author);
                ctx.title = Some(title);
            } else {
                // If it looks like an author name (≤5 words), set as author.
                // Otherwise it's probably a book/series title — set as title too
                // so generic filenames can fall back to it.
                let word_count = cleaned.split_whitespace().count();
                if word_count <= 5 {
                    ctx.author = Some(cleaned.clone());
                }
                ctx.title = Some(cleaned);
            }
        }
    }

    ctx
}

/// Split "Author - Title" pattern. Returns (author, title) if the left side
/// looks like an author name (≤5 words, no file extension characters).
fn split_author_title(name: &str) -> Option<(String, String)> {
    let pos = name.find(" - ")?;
    let left = normalize_spaces(&name[..pos]);
    let right = normalize_spaces(&name[pos + 3..]);

    // Author should be ≤5 words and not contain extension-like characters
    let word_count = left.split_whitespace().count();
    if word_count == 0 || word_count > 5 || right.is_empty() {
        return None;
    }
    // Reject if left contains dots (likely a filename, not author)
    if left.contains('.') && !left.contains("Jr.") && !left.contains("Sr.") {
        return None;
    }

    Some((left, right))
}

/// Detect "Series N - Title" pattern in a directory name.
///
/// Returns `(series, index, title)` if the left side of ` - ` ends with a
/// number and doesn't look like an author name (authors rarely end with digits).
///
/// Examples:
/// - `"8 Bit Warrior 2 - From Seeds to Swords"` → `("8 Bit Warrior", "2", "From Seeds to Swords")`
/// - `"Chronicles of Narnia 1 - The Lion..."` → `("Chronicles of Narnia", "1", "The Lion...")`
/// - `"Brandon Sanderson - Mistborn"` → `None` (left side doesn't end with digit)
fn split_series_title(name: &str) -> Option<(String, String, String)> {
    let pos = name.find(" - ")?;
    let left = normalize_spaces(&name[..pos]);
    let right = normalize_spaces(&name[pos + 3..]);

    if right.is_empty() {
        return None;
    }

    // Left side must end with a digit
    let last_char = left.chars().last()?;
    if !last_char.is_ascii_digit() {
        return None;
    }

    // Split off the trailing number from the left side
    let left_trimmed = left.trim_end_matches(|c: char| c.is_ascii_digit());
    let index_str = &left[left_trimmed.len()..];
    let series = normalize_spaces(left_trimmed);

    if series.is_empty() || index_str.is_empty() {
        return None;
    }

    // Strip leading zeros from index
    let index = index_str.trim_start_matches('0');
    let index = if index.is_empty() { "0" } else { index };

    Some((series, index.to_string(), right))
}

/// Result of parsing a multi-dash audiobook directory.
#[derive(Debug, Clone, Default)]
struct MultiDashParsed {
    author: Option<String>,
    series: Option<String>,
    series_position: Option<String>,
    title: Option<String>,
    narrator: Option<String>,
}

/// Series-code index regex: extract trailing digits from codes like "BC 005".
static SERIES_CODE_INDEX_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?:^|\s)0*(\d+)$").unwrap());

/// Parse directories with 3+ dash-separated sections.
///
/// Common patterns:
/// - 3 sections: `"Author - SeriesCode - Title"`
/// - 4 sections: `"Author - SeriesCode - Title - Narrator (Format)"`
///
/// The first section is treated as author if it's ≤5 words and looks like a
/// person name. The last section may be a narrator if it's a short name-like
/// string (possibly followed by a format tag in parens).
fn parse_multi_dash_audiobook_dir(name: &str) -> Option<MultiDashParsed> {
    let sections: Vec<&str> = name.split(" - ").collect();
    if sections.len() < 3 {
        return None;
    }

    let mut parsed = MultiDashParsed::default();

    // First section = author (if ≤5 words, looks name-like)
    let first = normalize_spaces(sections[0]);
    let word_count = first.split_whitespace().count();
    if word_count > 0 && word_count <= 5 && !first.contains('.') {
        parsed.author = Some(first);
    }

    // Last section: check if it's a narrator (short name, possibly with format tag)
    let last_raw = sections[sections.len() - 1];
    let last_clean = normalize_spaces(last_raw);

    // Strip trailing format paren like "(Ch)" or "(mp3)" from last section
    let last_no_paren = super::common::TRAILING_PAREN_RE
        .replace(&last_clean, "")
        .trim()
        .to_string();

    let last_is_narrator = {
        let words: Vec<&str> = last_no_paren.split_whitespace().collect();
        words.len() <= 2
            && !words.is_empty()
            && words[0]
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_uppercase())
    };

    // Determine which sections are series/title
    let (middle_sections, title_idx) = if last_is_narrator && sections.len() >= 4 {
        parsed.narrator = Some(last_no_paren);
        // Middle = series code(s), second-to-last = title
        (&sections[1..sections.len() - 2], sections.len() - 2)
    } else {
        // Middle = series code(s), last = title
        (&sections[1..sections.len() - 1], sections.len() - 1)
    };

    // Title
    parsed.title = Some(normalize_spaces(sections[title_idx]));

    // Middle sections → series code / series name
    if !middle_sections.is_empty() {
        let series_str = middle_sections
            .iter()
            .map(|s| normalize_spaces(s))
            .collect::<Vec<_>>()
            .join(" - ");

        // Extract index from series code (e.g., "BC 005" → position=5, series="BC")
        if let Some(caps) = SERIES_CODE_INDEX_RE.captures(&series_str) {
            parsed.series_position = Some(caps[1].to_string());
            let stripped = series_str[..caps.get(0).unwrap().start()].trim();
            if !stripped.is_empty() {
                parsed.series = Some(stripped.to_string());
            } else {
                parsed.series = Some(series_str);
            }
        } else {
            parsed.series = Some(series_str);
        }
    }

    Some(parsed)
}

/// Parse folder context for sport media.
///
/// Common patterns:
/// - `League/Year/filename.mkv` → title=League, year from Year folder
/// - `League/filename.mkv` → title=League
fn parse_sport_folder_context(components: &[&str]) -> FolderContext {
    let mut ctx = FolderContext::default();
    let depth = components.len();

    // Walk backwards from filename, looking for year folder and league folder
    for (i, component) in components.iter().enumerate().rev() {
        if i == depth - 1 {
            continue; // skip filename
        }

        // Check if this folder is a bare year (e.g., "2026")
        if ctx.year.is_none()
            && let Ok(year) = component.parse::<u32>()
            && (1900..2100).contains(&year)
        {
            ctx.year = Some(year);
            ctx.season = Some(year);
            continue;
        }

        // Check for season folder (Season XX) — some sport roots use this
        if let Some(caps) = SEASON_FOLDER_RE.captures(component) {
            if let Some(num) = caps.name("num") {
                ctx.season = num.as_str().parse().ok();
            }
            continue;
        }

        // First non-year/non-season folder is the league name
        if ctx.title.is_none() {
            let (title, year) = extract_title_and_year(component);
            ctx.title = Some(title);
            if year.is_some() && ctx.year.is_none() {
                ctx.year = year;
            }
        }
    }

    ctx
}

/// Extract title and year from a folder name.
fn extract_title_and_year(folder: &str) -> (String, Option<u32>) {
    // Extract year in parentheses/brackets
    let year = YEAR_FOLDER_RE
        .captures(folder)
        .and_then(|c| c.name("year"))
        .and_then(|m| m.as_str().parse().ok());

    // Remove year from title
    let title = YEAR_FOLDER_RE.replace_all(folder, "");
    // Replace " - " separator (filesystem-safe colon replacement) with space
    let title = title.replace(" - ", " ");
    let title = normalize_spaces(&title);

    (title, year)
}

/// Apply TV folder context to a parsed TV release.
fn resolve_tv_context(parsed: &mut ParsedRelease, context: &FolderContext) {
    if let ParsedMedia::Tv(ref mut tv) = parsed.media {
        // Use folder title if file title is poor
        if context.title.is_some() && (tv.show_name.is_empty() || tv.show_name.len() < 3) {
            tv.show_name = context.title.clone().unwrap_or_default();
            parsed.title = tv.show_name.clone();
        }

        // Use folder season if not detected in filename
        if tv.season.is_none() && context.season.is_some() {
            tv.season = context.season;
            if context.season == Some(0) {
                tv.is_special = true;
            }
        }

        // Use folder year if not detected
        if tv.year.is_none() && context.year.is_some() {
            tv.year = context.year;
        }
    }
}

/// Apply movie folder context to a parsed movie release.
fn resolve_movie_context(parsed: &mut ParsedRelease, context: &FolderContext) {
    if let ParsedMedia::Movie(ref mut movie) = parsed.media {
        if let Some(folder_title) = &context.title {
            // Use folder title when:
            // - file title is very short (< 3 chars), OR
            // - folder has a year but filename doesn't (folder is more informative,
            //   filename is likely an obfuscated scene release name), OR
            // - folder and filename titles share significant word overlap, meaning they
            //   describe the same movie but the folder has a curated/complete title
            //   (e.g. "Dead Like Me - Life After Death/" vs "dead.like.me.movie.dvdrip.mkv")
            let folder_more_informative = movie.title.len() < 3
                || (context.year.is_some() && movie.year.is_none())
                || folder_title_is_better(folder_title, &movie.title);

            if folder_more_informative {
                movie.title = folder_title.clone();
                parsed.title = movie.title.clone();
            }
        }

        // Use folder year if not detected in filename
        if movie.year.is_none() && context.year.is_some() {
            movie.year = context.year;
        }
    }
}

/// Check if a folder title is a better version of the filename title.
///
/// Returns true when the folder and filename titles share most words (same movie)
/// but the folder title is longer/more complete. This catches scene releases in
/// curated folders, e.g. folder "Dead Like Me Life After Death" vs filename
/// "dead like me movie".
fn folder_title_is_better(folder_title: &str, file_title: &str) -> bool {
    let folder_words: Vec<&str> = folder_title.split_whitespace().collect();
    let file_words: Vec<&str> = file_title.split_whitespace().collect();

    // Need at least 2 words in each to compare meaningfully
    if folder_words.len() < 2 || file_words.len() < 2 {
        return false;
    }

    // Folder must be at least as long as file title to be "better"
    if folder_words.len() <= file_words.len() {
        return false;
    }

    // Count how many file title words appear in the folder title (case-insensitive)
    let matching = file_words
        .iter()
        .filter(|fw| folder_words.iter().any(|dw| dw.eq_ignore_ascii_case(fw)))
        .count();

    // If most file words (>= 50%) appear in the folder title, the folder is a
    // more complete version of the same title
    matching * 2 >= file_words.len()
}

/// Apply book folder context to a parsed book release.
///
/// For audiobooks, detects generic filenames (e.g. `01.mp3`, `track05.mp3`) and
/// replaces the title with folder-derived context. Sets `is_generic_filename`
/// so downstream consumers know the title isn't from the actual filename.
fn resolve_book_context(parsed: &mut ParsedRelease, context: &FolderContext, filename: &str) {
    match &mut parsed.media {
        ParsedMedia::Book(book) => {
            if book.author.is_none() && context.author.is_some() {
                book.author = context.author.clone();
            }
            if book.series.is_none() && context.series.is_some() {
                book.series = context.series.clone();
            }
        }
        ParsedMedia::Audiobook(ab) => {
            // Check if the raw filename (before parser cleaning) is generic,
            // or if the parsed title is purely digits (e.g., "Author - 001.mp3"
            // where the parser split off "001" as the title)
            let stem = remove_extension(filename);
            let is_generic = is_generic_audiobook_filename(&stem)
                || ab.title.chars().all(|c| c.is_ascii_digit());

            if is_generic {
                ab.is_generic_filename = true;

                // Replace title with folder-derived title or series
                if let Some(folder_title) = &context.title {
                    let clean = strip_part_suffix(folder_title);
                    ab.title = clean.clone();
                    parsed.title = clean;
                } else if let Some(series) = &context.series {
                    let clean = strip_part_suffix(series);
                    ab.title = clean.clone();
                    parsed.title = clean;
                }
            }

            // Fill gaps from folder context
            if ab.author.is_none() && context.author.is_some() {
                ab.author = context.author.clone();
            }
            if ab.narrator.is_none() && context.narrator.is_some() {
                ab.narrator = context.narrator.clone();
            }
            if ab.series.is_none() && context.series.is_some() {
                ab.series = context.series.clone();
            }
            if ab.series_position.is_none() && context.series_position.is_some() {
                ab.series_position = context.series_position.clone();
            }
            if ab.year.is_none() && context.year.is_some() {
                ab.year = context.year;
            }

            // Clear series if it's just echoing the title — folder structure
            // like Author/Title/file.mp3 sets the parent as series, but when
            // it matches the title it's not real series data.
            // Only do this when the title came from the filename (not from
            // folder fallback for generic filenames).
            if !ab.is_generic_filename
                && let Some(ref series) = ab.series
                && series.eq_ignore_ascii_case(&ab.title)
            {
                ab.series = None;
                ab.series_position = None;
            }
        }
        _ => {}
    }
}

/// Apply sport folder context to a parsed sport release.
fn resolve_sport_context(parsed: &mut ParsedRelease, context: &FolderContext) {
    if let ParsedMedia::Sport(ref mut sport) = parsed.media {
        // Use folder title as league name if filename parse didn't get one
        if context.title.is_some() && sport.league_name.is_empty() {
            sport.league_name = context.title.clone().unwrap_or_default();
            parsed.title = sport.league_name.clone();
        }

        // Use folder year as season if not detected in filename
        if sport.season.is_none() && context.year.is_some() {
            sport.season = context.year;
        }
        if sport.year.is_none() && context.year.is_some() {
            sport.year = context.year;
        }
    }
}

/// Resolve parsed file data with folder context.
fn resolve_with_context(
    mut parsed: ParsedRelease,
    context: &FolderContext,
    media_kind: &str,
    filename: &str,
) -> ParsedRelease {
    match media_kind {
        "tv" => resolve_tv_context(&mut parsed, context),
        "movies" => resolve_movie_context(&mut parsed, context),
        "music" => {
            if let ParsedMedia::Music(ref mut music) = parsed.media {
                // Use folder artist if not detected
                if context.artist.is_some() && music.artist.is_empty() {
                    music.artist = context.artist.clone().unwrap_or_default();
                    parsed.title = music.artist.clone();
                }

                // Use folder album if not detected
                if music.album.is_none() && context.album.is_some() {
                    music.album = context.album.clone();
                }

                // Use folder year
                if music.year.is_none() && context.year.is_some() {
                    music.year = context.year;
                }
            }
        }
        "ebooks" | "audiobooks" => resolve_book_context(&mut parsed, context, filename),
        "sports" => resolve_sport_context(&mut parsed, context),
        _ => {
            // Auto-detect: dispatch based on what the parser detected
            match &parsed.media {
                ParsedMedia::Tv(_) => resolve_tv_context(&mut parsed, context),
                ParsedMedia::Movie(_) => resolve_movie_context(&mut parsed, context),
                ParsedMedia::Book(_) | ParsedMedia::Audiobook(_) => {
                    resolve_book_context(&mut parsed, context, filename)
                }
                ParsedMedia::Sport(_) => resolve_sport_context(&mut parsed, context),
                _ => {}
            }
        }
    }

    parsed
}

/// Return empty result when path has no components.
fn empty_path_result(media_kind: &str) -> PathParsed {
    let parsed = parse_release("", media_kind);
    PathParsed {
        folder_context: FolderContext::default(),
        file_parsed: parsed.clone(),
        resolved: parsed,
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tv_path() {
        let result = parse_path("Game of Thrones/Season 01/S01E05.mkv", "tv");

        assert_eq!(
            result.folder_context.title.as_deref(),
            Some("Game of Thrones")
        );
        assert_eq!(result.folder_context.season, Some(1));

        if let ParsedMedia::Tv(tv) = &result.resolved.media {
            assert_eq!(tv.season, Some(1));
            assert_eq!(tv.episodes, vec![5]);
        } else {
            panic!("Expected TV");
        }
    }

    #[test]
    fn test_tv_path_with_year() {
        let result = parse_path("Doctor Who (2005)/Season 01/S01E01.mkv", "tv");

        assert_eq!(result.folder_context.year, Some(2005));

        if let ParsedMedia::Tv(tv) = &result.resolved.media {
            assert_eq!(tv.year, Some(2005));
        } else {
            panic!("Expected TV");
        }
    }

    #[test]
    fn test_tv_specials_folder() {
        let result = parse_path("Show Name/Specials/S00E01.mkv", "tv");

        assert_eq!(result.folder_context.season, Some(0));

        if let ParsedMedia::Tv(tv) = &result.resolved.media {
            assert!(tv.is_special);
        } else {
            panic!("Expected TV");
        }
    }

    #[test]
    fn test_movie_path() {
        let result = parse_path("Inception (2010)/Inception.2010.1080p.BluRay.mkv", "movies");

        assert_eq!(result.folder_context.title.as_deref(), Some("Inception"));
        assert_eq!(result.folder_context.year, Some(2010));
    }

    #[test]
    fn test_music_path() {
        let result = parse_path("Pink Floyd/The Wall (1979)/01 - In The Flesh.flac", "music");

        assert_eq!(result.folder_context.artist.as_deref(), Some("Pink Floyd"));
        assert_eq!(result.folder_context.album.as_deref(), Some("The Wall"));
        assert_eq!(result.folder_context.year, Some(1979));
    }

    #[test]
    fn test_movie_obfuscated_filename_uses_folder() {
        // Obfuscated release: filename has no year, folder does → prefer folder
        // " - " in folder is a filesystem-safe colon, gets normalized to space
        let result = parse_path(
            "Anchorman 2 - The Legend Continues (2013)/blow-anchormantwo720.mkv",
            "movies",
        );

        if let ParsedMedia::Movie(movie) = &result.resolved.media {
            assert_eq!(movie.title, "Anchorman 2 The Legend Continues");
            assert_eq!(movie.year, Some(2013));
        } else {
            panic!("Expected Movie");
        }
    }

    #[test]
    fn test_movie_scene_file_in_curated_folder_uses_folder() {
        // Scene release in a properly-named folder: folder has the real title,
        // filename has an abbreviated version with "movie" tag
        let result = parse_path(
            "Dead Like Me - Life After Death/dead.like.me.movie.dvdrip.x264-osiris.mkv",
            "movies",
        );

        if let ParsedMedia::Movie(movie) = &result.resolved.media {
            assert_eq!(movie.title, "Dead Like Me Life After Death");
        } else {
            panic!("Expected Movie");
        }
    }

    #[test]
    fn test_movie_unrelated_folder_keeps_filename() {
        // Folder name is unrelated (group name) — should keep filename title
        let result = parse_path("YIFY/Inception.2010.1080p.BluRay.mkv", "movies");

        if let ParsedMedia::Movie(movie) = &result.resolved.media {
            assert_eq!(movie.title, "Inception");
            assert_eq!(movie.year, Some(2010));
        } else {
            panic!("Expected Movie");
        }
    }

    #[test]
    fn test_movie_good_filename_keeps_filename() {
        // Non-obfuscated: both filename and folder have year → filename is fine
        let result = parse_path("Inception (2010)/Inception.2010.1080p.BluRay.mkv", "movies");

        if let ParsedMedia::Movie(movie) = &result.resolved.media {
            assert_eq!(movie.title, "Inception");
            assert_eq!(movie.year, Some(2010));
        } else {
            panic!("Expected Movie");
        }
    }

    #[test]
    fn test_minimal_filename_uses_folder() {
        let result = parse_path("Show Name/Season 01/E05.mkv", "tv");

        // Even though filename has no show name, resolved should have it
        if let ParsedMedia::Tv(tv) = &result.resolved.media {
            assert!(
                tv.show_name.contains("Show Name") || result.resolved.title.contains("Show Name")
            );
        } else {
            panic!("Expected TV");
        }
    }

    #[test]
    fn test_book_path_author_series() {
        let result = parse_path("Brandon Sanderson/Mistborn/The Final Empire.epub", "ebooks");

        assert_eq!(
            result.folder_context.author.as_deref(),
            Some("Brandon Sanderson")
        );
        assert_eq!(result.folder_context.series.as_deref(), Some("Mistborn"));

        if let ParsedMedia::Book(book) = &result.resolved.media {
            assert_eq!(book.author.as_deref(), Some("Brandon Sanderson"));
            assert_eq!(book.series.as_deref(), Some("Mistborn"));
        } else {
            panic!("Expected Book, got {:?}", result.resolved.media);
        }
    }

    #[test]
    fn test_book_path_author_only() {
        let result = parse_path("Brandon Sanderson/Mistborn.epub", "ebooks");

        assert_eq!(
            result.folder_context.author.as_deref(),
            Some("Brandon Sanderson")
        );
        assert!(result.folder_context.series.is_none());
    }

    /// Test parse_path against real file paths from the test-data directory.
    ///
    /// Paths are relative to the media root (TV/ or Movies/), matching what
    /// a media root scanner would produce.
    #[test]
    fn test_real_test_data_files() {
        struct TvCase {
            path: &'static str,
            show_name: &'static str,
            season: u32,
            episodes: &'static [u32],
        }

        struct MovieCase {
            path: &'static str,
            title_contains: &'static str,
            year: u32,
        }

        let tv_cases = [
            TvCase {
                path: "Barry/Season 01/Barry - 1x01 - Chapter One- Make Your Mark.mkv",
                show_name: "Barry",
                season: 1,
                episodes: &[1],
            },
            TvCase {
                path: "Barry/Season 01/Barry - 1x06 - Chapter Six- Listen with Your Ears, React with Your Face.mkv",
                show_name: "Barry",
                season: 1,
                episodes: &[6],
            },
            TvCase {
                path: "Barry/Season 03/Barry - 3x05 - crazytimesh-tshow.mkv",
                show_name: "Barry",
                season: 3,
                episodes: &[5],
            },
            TvCase {
                path: "Barry/Season 04/Barry - S04E05 - tricky legacies.mkv",
                show_name: "Barry",
                season: 4,
                episodes: &[5],
            },
            TvCase {
                path: "Shrinking/Season 01/Shrinking - 1x04 - Potatoes.mkv",
                show_name: "Shrinking",
                season: 1,
                episodes: &[4],
            },
            TvCase {
                path: "Shrinking/Season 01/Shrinking - S01E10 - Closure.mkv",
                show_name: "Shrinking",
                season: 1,
                episodes: &[10],
            },
            TvCase {
                path: "Shrinking/Season 02/Shrinking - S02E12 - The Last Thanksgiving.mkv",
                show_name: "Shrinking",
                season: 2,
                episodes: &[12],
            },
            TvCase {
                path: "Shrinking/Season 1/Shrinking - S01E09 - Moving Forward.mkv",
                show_name: "Shrinking",
                season: 1,
                episodes: &[9],
            },
        ];

        let movie_cases = [
            MovieCase {
                path: "Anchorman 2 - The Legend Continues (2013)/blow-anchormantwo720.mkv",
                title_contains: "Anchorman",
                year: 2013,
            },
            MovieCase {
                path: "Anchorman The Legend of Ron Burgundy (2004)/Anchorman The Legend of Ron Burgundy.mkv",
                title_contains: "Anchorman",
                year: 2004,
            },
        ];

        let mut failures = Vec::new();

        // --- TV cases ---
        for (i, tc) in tv_cases.iter().enumerate() {
            let result = parse_path(tc.path, "tv");
            let tv = match &result.resolved.media {
                ParsedMedia::Tv(tv) => tv,
                other => {
                    let msg = format!(
                        "TV[{}] {}: expected Tv variant, got {:?}",
                        i, tc.path, other
                    );
                    println!("FAIL: {msg}");
                    failures.push(msg);
                    continue;
                }
            };

            let mut case_ok = true;

            if tv.show_name != tc.show_name {
                let msg = format!(
                    "TV[{}] {}: show_name = {:?}, expected {:?}",
                    i, tc.path, tv.show_name, tc.show_name
                );
                println!("FAIL: {msg}");
                failures.push(msg);
                case_ok = false;
            }

            if tv.season != Some(tc.season) {
                let msg = format!(
                    "TV[{}] {}: season = {:?}, expected Some({})",
                    i, tc.path, tv.season, tc.season
                );
                println!("FAIL: {msg}");
                failures.push(msg);
                case_ok = false;
            }

            if tv.episodes.as_slice() != tc.episodes {
                let msg = format!(
                    "TV[{}] {}: episodes = {:?}, expected {:?}",
                    i, tc.path, tv.episodes, tc.episodes
                );
                println!("FAIL: {msg}");
                failures.push(msg);
                case_ok = false;
            }

            if case_ok {
                println!(
                    "OK:   TV[{}] {} -> show={:?} season={:?} eps={:?}",
                    i, tc.path, tv.show_name, tv.season, tv.episodes
                );
            }
        }

        // --- Movie cases ---
        for (i, mc) in movie_cases.iter().enumerate() {
            let result = parse_path(mc.path, "movies");
            let movie = match &result.resolved.media {
                ParsedMedia::Movie(movie) => movie,
                other => {
                    let msg = format!(
                        "Movie[{}] {}: expected Movie variant, got {:?}",
                        i, mc.path, other
                    );
                    println!("FAIL: {msg}");
                    failures.push(msg);
                    continue;
                }
            };

            let mut case_ok = true;

            if !movie.title.contains(mc.title_contains) {
                let msg = format!(
                    "Movie[{}] {}: title = {:?}, expected to contain {:?}",
                    i, mc.path, movie.title, mc.title_contains
                );
                println!("FAIL: {msg}");
                failures.push(msg);
                case_ok = false;
            }

            if movie.year != Some(mc.year) {
                let msg = format!(
                    "Movie[{}] {}: year = {:?}, expected Some({})",
                    i, mc.path, movie.year, mc.year
                );
                println!("FAIL: {msg}");
                failures.push(msg);
                case_ok = false;
            }

            if case_ok {
                println!(
                    "OK:   Movie[{}] {} -> title={:?} year={:?}",
                    i, mc.path, movie.title, movie.year
                );
            }
        }

        if !failures.is_empty() {
            panic!(
                "\n{} parse failures:\n  - {}\n",
                failures.len(),
                failures.join("\n  - ")
            );
        }
    }

    #[test]
    fn test_sport_path_with_year_folder() {
        let result = parse_path(
            "MotoGP/2026/MotoGP - R01 - Thailand Grand Prix Race.mkv",
            "sports",
        );

        // Folder context should extract league and year
        assert_eq!(result.folder_context.title.as_deref(), Some("MotoGP"));
        assert_eq!(result.folder_context.year, Some(2026));

        // Resolved should have sport media with all fields
        if let ParsedMedia::Sport(sport) = &result.resolved.media {
            assert_eq!(sport.league_name, "MotoGP");
            assert_eq!(sport.season, Some(2026));
            assert_eq!(sport.round, Some(1));
            assert_eq!(sport.event_title.as_deref(), Some("Thailand Grand Prix"));
            assert_eq!(sport.session_type.as_deref(), Some("Race"));
        } else {
            panic!("Expected Sport, got {:?}", result.resolved.media);
        }
    }

    #[test]
    fn test_sport_path_release_name_format() {
        let result = parse_path(
            "MotoGP/2024/MotoGP.2024.R01.Thailand.Race.1080p.WEB-DL.mkv",
            "sports",
        );

        if let ParsedMedia::Sport(sport) = &result.resolved.media {
            assert_eq!(sport.league_name, "MotoGP");
            assert_eq!(sport.season, Some(2024));
            assert_eq!(sport.round, Some(1));
            assert_eq!(sport.location.as_deref(), Some("Thailand"));
            assert_eq!(sport.session_type.as_deref(), Some("Race"));
        } else {
            panic!("Expected Sport, got {:?}", result.resolved.media);
        }
    }

    // =========================================================================
    // Audiobook path tests
    // =========================================================================

    #[test]
    fn test_audiobook_path_author_folder() {
        // Author/Title/01.mp3 — author from grandparent
        let result = parse_path("Brandon Sanderson/Mistborn/01.mp3", "audiobooks");
        if let ParsedMedia::Audiobook(ab) = &result.resolved.media {
            assert_eq!(ab.author.as_deref(), Some("Brandon Sanderson"));
        } else {
            panic!("Expected Audiobook, got {:?}", result.resolved.media);
        }
    }

    #[test]
    fn test_audiobook_path_single_folder() {
        // Title/01.mp3 — no author from folder (only 2 components)
        let result = parse_path("Mistborn/01.mp3", "audiobooks");
        assert_eq!(result.folder_context.author.as_deref(), Some("Mistborn"));
        // Author context gets applied (parent = author for 2 components)
        if let ParsedMedia::Audiobook(ab) = &result.resolved.media {
            assert_eq!(ab.author.as_deref(), Some("Mistborn"));
        } else {
            panic!("Expected Audiobook, got {:?}", result.resolved.media);
        }
    }

    #[test]
    fn test_audiobook_path_author_title_filename() {
        // "Author - Title.mp3" in a folder
        let result = parse_path("Clive Barker - The Thief of Always.mp3", "audiobooks");
        if let ParsedMedia::Audiobook(ab) = &result.resolved.media {
            assert_eq!(ab.author.as_deref(), Some("Clive Barker"));
            assert!(
                ab.title.contains("Thief of Always"),
                "title should contain book name, got: {}",
                ab.title
            );
        } else {
            panic!("Expected Audiobook, got {:?}", result.resolved.media);
        }
    }

    // =========================================================================
    // Audiobook: generic filename → folder title fallback
    // =========================================================================

    #[test]
    fn test_audiobook_generic_filename_uses_folder_title() {
        // Generic "01.mp3" → title comes from folder
        let result = parse_path(
            "13 Storey Treehouse/0001_13storeyTreehouse.mp3",
            "audiobooks",
        );
        if let ParsedMedia::Audiobook(ab) = &result.resolved.media {
            assert!(ab.is_generic_filename, "should be flagged as generic");
            assert!(
                ab.title.contains("Storey Treehouse"),
                "title should come from folder, got: {}",
                ab.title
            );
        } else {
            panic!("Expected Audiobook, got {:?}", result.resolved.media);
        }
    }

    #[test]
    fn test_audiobook_generic_digit_only_uses_folder() {
        let result = parse_path("Brandon Sanderson/Mistborn/01.mp3", "audiobooks");
        if let ParsedMedia::Audiobook(ab) = &result.resolved.media {
            assert!(ab.is_generic_filename);
            assert_eq!(ab.author.as_deref(), Some("Brandon Sanderson"));
            // Title should fall back to series (parent folder)
            assert!(
                ab.title.contains("Mistborn"),
                "title should come from folder, got: {}",
                ab.title
            );
        } else {
            panic!("Expected Audiobook, got {:?}", result.resolved.media);
        }
    }

    // =========================================================================
    // Audiobook: Author - Title directory split
    // =========================================================================

    #[test]
    fn test_audiobook_series_index_dir_split() {
        let result = parse_path(
            "8 Bit Warrior 2 - From Seeds to Swords/01.mp3",
            "audiobooks",
        );
        if let ParsedMedia::Audiobook(ab) = &result.resolved.media {
            assert!(ab.is_generic_filename);
            // Now detected as series+index pattern
            assert!(
                ab.title.contains("From Seeds to Swords"),
                "title should be from dir split, got: {}",
                ab.title
            );
            assert_eq!(
                ab.series.as_deref(),
                Some("8 Bit Warrior"),
                "series should be '8 Bit Warrior'"
            );
            assert_eq!(
                ab.series_position.as_deref(),
                Some("2"),
                "series_position should be '2'"
            );
        } else {
            panic!("Expected Audiobook, got {:?}", result.resolved.media);
        }
    }

    // =========================================================================
    // Audiobook: noisy directory stripping
    // =========================================================================

    #[test]
    fn test_audiobook_noisy_scene_dir() {
        let result = parse_path(
            "1992 - The Thief of Always (Sandvold) 64k 04.10.40 {120mb}/Thief of Always 01.mp3",
            "audiobooks",
        );
        if let ParsedMedia::Audiobook(ab) = &result.resolved.media {
            assert!(
                ab.title.contains("Thief of Always"),
                "title should contain book name, got: {}",
                ab.title
            );
            assert_eq!(ab.year, Some(1992), "year should be extracted from dir");
            assert_eq!(
                ab.narrator.as_deref(),
                Some("Sandvold"),
                "narrator should be extracted from trailing parens"
            );
        } else {
            panic!("Expected Audiobook, got {:?}", result.resolved.media);
        }
    }

    // =========================================================================
    // Audiobook: dot-separated scene release grandparent
    // =========================================================================

    #[test]
    fn test_audiobook_scene_release_grandparent() {
        let result = parse_path(
            "JEFF.KINNEY-DIARY.OF.A.WIMPY.KID.SERIES.1.9af496-17/1. Diary of a Wimpy Kid/Diary of a Wimpy Kid-Part01.mp3",
            "audiobooks",
        );
        if let ParsedMedia::Audiobook(ab) = &result.resolved.media {
            assert_eq!(ab.author.as_deref(), Some("Jeff Kinney"));
        } else {
            panic!("Expected Audiobook, got {:?}", result.resolved.media);
        }
    }

    #[test]
    fn test_audiobook_scene_george_martin() {
        let result = parse_path(
            "George.R.R.Martin-A.song.of.ice.and.fire-book.1-A.Game.of.Thrones-ObfuscatedS/subfolder/ASOIAF - Book 1 - A Game of Thrones - 35 - Eddard IX.mp3",
            "audiobooks",
        );
        // Grandparent is the dot-separated dir
        assert_eq!(
            result.folder_context.author.as_deref(),
            Some("George R.R. Martin")
        );
    }

    // =========================================================================
    // Audiobook: bracket tags stripped from dirs
    // =========================================================================

    #[test]
    fn test_audiobook_asin_bracket_stripped() {
        let result = parse_path(
            "Harry Potter and the Philosopher's Stone (Full-Cast Edition) [B0F14RFHS6] [EC-3]/Chapter 01.mp3",
            "audiobooks",
        );
        if let ParsedMedia::Audiobook(ab) = &result.resolved.media {
            assert!(ab.is_generic_filename);
            // Bracket tags should be stripped, leaving the title
            assert!(
                ab.title.contains("Harry Potter"),
                "title should contain book name, got: {}",
                ab.title
            );
        } else {
            panic!("Expected Audiobook, got {:?}", result.resolved.media);
        }
    }

    // =========================================================================
    // Audiobook: good filename keeps filename title
    // =========================================================================

    #[test]
    fn test_audiobook_good_filename_keeps_it() {
        let result = parse_path(
            "01 A Shadow in the Ember/Flesh and Fire Book 1 - A Shadow in the Ember.m4b",
            "audiobooks",
        );
        if let ParsedMedia::Audiobook(ab) = &result.resolved.media {
            assert!(!ab.is_generic_filename);
            // Good filename — should use filename-derived title
            assert!(
                ab.title.contains("Shadow in the Ember") || ab.title.contains("Flesh and Fire"),
                "title should come from filename, got: {}",
                ab.title
            );
        } else {
            panic!("Expected Audiobook, got {:?}", result.resolved.media);
        }
    }

    // =========================================================================
    // Audiobook: author-title in grandparent dir
    // =========================================================================

    #[test]
    fn test_audiobook_author_title_grandparent() {
        let result = parse_path(
            "Lemony Snicket - A Series of Unfortunate Events/Book The 5th- The Austere Academy/Chapter 09.mp3",
            "audiobooks",
        );
        if let ParsedMedia::Audiobook(ab) = &result.resolved.media {
            assert!(ab.is_generic_filename);
            // Grandparent splits: "Lemony Snicket" is NOT treated as author here
            // because grandparent doesn't use split_author_title, only parent does.
            // But the grandparent after noise stripping becomes the author.
            // Actually: "Lemony Snicket - A Series of Unfortunate Events" is the grandparent.
            // After clean_folder_name + dot parsing fails, it becomes the raw author.
            // Let's just check the series is set from parent.
            assert!(ab.series.is_some(), "series should be set from parent dir");
        } else {
            panic!("Expected Audiobook, got {:?}", result.resolved.media);
        }
    }

    // =========================================================================
    // Audiobook: single file, no directory context
    // =========================================================================

    #[test]
    fn test_audiobook_single_file_by_author() {
        let result = parse_path(
            "A Kingdom of Flesh and Fire by Jennifer L. Armentrout.m4b",
            "audiobooks",
        );
        if let ParsedMedia::Audiobook(ab) = &result.resolved.media {
            assert_eq!(ab.author.as_deref(), Some("Jennifer L. Armentrout"));
            assert!(
                ab.title.contains("Kingdom of Flesh and Fire"),
                "title should contain book name, got: {}",
                ab.title
            );
        } else {
            panic!("Expected Audiobook, got {:?}", result.resolved.media);
        }
    }

    // =========================================================================
    // Audiobook: series notation in filename
    // =========================================================================

    #[test]
    fn test_audiobook_series_in_filename() {
        let result = parse_path(
            "A Soul of Ash and Blood/Jennifer L. Armentrout - A Soul of Ash and Blood - Blood and Ash #5.m4b",
            "audiobooks",
        );
        if let ParsedMedia::Audiobook(ab) = &result.resolved.media {
            assert_eq!(ab.author.as_deref(), Some("Jennifer L. Armentrout"));
            assert!(
                ab.title.contains("Soul of Ash and Blood"),
                "title was: {}",
                ab.title
            );
        } else {
            panic!("Expected Audiobook, got {:?}", result.resolved.media);
        }
    }

    // =========================================================================
    // Audiobook: multi-dash Boxcar pattern
    // =========================================================================

    #[test]
    fn test_audiobook_multi_dash_boxcar() {
        let result = parse_path(
            "Boxcar Children 1-138/Gertrude Chandler Warner - BC 005 - Mike's Mystery - Gregory (Ch)/track01.mp3",
            "audiobooks",
        );
        if let ParsedMedia::Audiobook(ab) = &result.resolved.media {
            assert!(ab.is_generic_filename);
            assert_eq!(
                ab.author.as_deref(),
                Some("Gertrude Chandler Warner"),
                "author from multi-dash parent"
            );
            assert!(
                ab.title.as_str().contains("Mike's Mystery"),
                "title should be Mike's Mystery, got: {}",
                ab.title
            );
            assert_eq!(
                ab.narrator.as_deref(),
                Some("Gregory"),
                "narrator from last section"
            );
            assert_eq!(
                ab.series_position.as_deref(),
                Some("5"),
                "series_position from BC 005"
            );
        } else {
            panic!("Expected Audiobook, got {:?}", result.resolved.media);
        }
    }

    // =========================================================================
    // Audiobook: Author/Title still works
    // =========================================================================

    #[test]
    fn test_audiobook_author_title_still_works() {
        let result = parse_path(
            "Andy Griffiths/The 26-Storey Treehouse/0001_treehouse.mp3",
            "audiobooks",
        );
        if let ParsedMedia::Audiobook(ab) = &result.resolved.media {
            assert_eq!(
                ab.author.as_deref(),
                Some("Andy Griffiths"),
                "author from grandparent"
            );
            assert!(ab.is_generic_filename);
            // Title should come from series (parent dir) since filename is generic
            assert!(
                ab.title.contains("Storey Treehouse"),
                "title should come from folder context, got: {}",
                ab.title
            );
        } else {
            panic!("Expected Audiobook, got {:?}", result.resolved.media);
        }
    }

    // =========================================================================
    // split_series_title unit tests
    // =========================================================================

    #[test]
    fn test_split_series_title_basic() {
        let result = split_series_title("8 Bit Warrior 2 - From Seeds to Swords");
        assert_eq!(
            result,
            Some((
                "8 Bit Warrior".to_string(),
                "2".to_string(),
                "From Seeds to Swords".to_string()
            ))
        );
    }

    #[test]
    fn test_split_series_title_no_match_author() {
        // Author name doesn't end with digit
        assert!(split_series_title("Brandon Sanderson - Mistborn").is_none());
    }

    #[test]
    fn test_split_series_title_leading_zeros() {
        let result = split_series_title("Series Name 001 - Title Here");
        assert_eq!(
            result,
            Some((
                "Series Name".to_string(),
                "1".to_string(),
                "Title Here".to_string()
            ))
        );
    }

    // =========================================================================
    // parse_multi_dash_audiobook_dir unit tests
    // =========================================================================

    #[test]
    fn test_multi_dash_3_sections() {
        let result =
            parse_multi_dash_audiobook_dir("Author Name - Series Code - Title Here").unwrap();
        assert_eq!(result.author.as_deref(), Some("Author Name"));
        assert_eq!(result.title.as_deref(), Some("Title Here"));
        assert_eq!(result.series.as_deref(), Some("Series Code"));
    }

    #[test]
    fn test_multi_dash_4_sections_narrator() {
        let result = parse_multi_dash_audiobook_dir(
            "Gertrude Chandler Warner - BC 005 - Mike's Mystery - Gregory (Ch)",
        )
        .unwrap();
        assert_eq!(result.author.as_deref(), Some("Gertrude Chandler Warner"));
        assert_eq!(result.title.as_deref(), Some("Mike's Mystery"));
        assert_eq!(result.narrator.as_deref(), Some("Gregory"));
        assert_eq!(result.series_position.as_deref(), Some("5"));
        // Series code should be stripped to just the prefix, not "BC 005"
        assert_eq!(result.series.as_deref(), Some("BC"));
    }

    #[test]
    fn test_multi_dash_series_code_strips_index() {
        // Pure numeric series code keeps original when no prefix
        let result = parse_multi_dash_audiobook_dir("Author - 005 - Title").unwrap();
        assert_eq!(result.series_position.as_deref(), Some("5"));
        assert_eq!(result.series.as_deref(), Some("005"));

        // Named series code strips trailing number
        let result = parse_multi_dash_audiobook_dir("Author - WC 012 - Title").unwrap();
        assert_eq!(result.series_position.as_deref(), Some("12"));
        assert_eq!(result.series.as_deref(), Some("WC"));

        // Multi-word series prefix
        let result = parse_multi_dash_audiobook_dir("Author - Box Set 03 - Title").unwrap();
        assert_eq!(result.series_position.as_deref(), Some("3"));
        assert_eq!(result.series.as_deref(), Some("Box Set"));
    }

    #[test]
    fn test_multi_dash_2_sections_returns_none() {
        assert!(parse_multi_dash_audiobook_dir("Author - Title").is_none());
    }

    /// Bulk sanity check: run ~/audiobooks.txt through parse_path and flag issues.
    ///
    /// This test reads the real audiobook file listing, filters to audio files,
    /// strips the media root prefix, and parses each path. It collects and prints
    /// issues for manual review.
    ///
    /// Run with: cargo test --lib parsing::path::tests::test_bulk_audiobooks -- --ignored --nocapture
    #[test]
    #[ignore] // Only run manually — depends on ~/audiobooks.txt existing
    fn test_bulk_audiobooks() {
        use std::collections::HashMap;
        use std::fs;

        let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
        let content = match fs::read_to_string(std::path::Path::new(&home).join("audiobooks.txt")) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("Skipping bulk test: {e}");
                return;
            }
        };

        let audio_exts = ["mp3", "m4b", "m4a", "flac", "ogg", "opus"];
        let media_root = std::env::var("KOBLOAD_TEST_MEDIA_ROOT")
            .unwrap_or_else(|_| "/media/audiobooks/".to_string());

        let mut total = 0;
        let mut issues: Vec<String> = Vec::new();
        let mut issue_counts: HashMap<&str, usize> = HashMap::new();

        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }

            // Filter to audio files only
            let ext = line.rsplit('.').next().unwrap_or("").to_lowercase();
            if !audio_exts.contains(&ext.as_str()) {
                continue;
            }

            // Strip media root prefix
            let rel_path = line.strip_prefix(&media_root).unwrap_or(line);

            let result = parse_path(rel_path, "audiobooks");
            total += 1;

            if let ParsedMedia::Audiobook(ab) = &result.resolved.media {
                // Flag: title is empty or pure digits
                if ab.title.is_empty() || ab.title.chars().all(|c| c.is_ascii_digit()) {
                    *issue_counts.entry("empty_or_digit_title").or_default() += 1;
                    issues.push(format!("EMPTY_TITLE: {} → title={:?}", rel_path, ab.title));
                }

                // Flag: author looks like a title (>5 words or starts with digit)
                if let Some(ref author) = ab.author {
                    let word_count = author.split_whitespace().count();
                    let starts_digit = author.chars().next().is_some_and(|c| c.is_ascii_digit());
                    if word_count > 5 || (starts_digit && word_count > 2) {
                        *issue_counts.entry("suspicious_author").or_default() += 1;
                        issues.push(format!(
                            "BAD_AUTHOR: {} → author={:?} title={:?}",
                            rel_path, author, ab.title
                        ));
                    }
                }

                // Flag: series set but no series_position
                if ab.series.is_some() && ab.series_position.is_none() {
                    *issue_counts.entry("series_no_position").or_default() += 1;
                    issues.push(format!(
                        "SERIES_NO_POS: {} → series={:?}",
                        rel_path, ab.series
                    ));
                }

                // Flag: generic filename but no folder-derived title
                if ab.is_generic_filename
                    && (ab.title.is_empty() || ab.title.chars().all(|c| c.is_ascii_digit()))
                {
                    *issue_counts.entry("generic_no_fallback").or_default() += 1;
                    issues.push(format!(
                        "GENERIC_NO_FALLBACK: {} → title={:?}",
                        rel_path, ab.title
                    ));
                }
            } else {
                *issue_counts.entry("not_audiobook").or_default() += 1;
                issues.push(format!(
                    "NOT_AUDIOBOOK: {} → {:?}",
                    rel_path, result.resolved.media
                ));
            }
        }

        // Print summary
        println!("\n=== Bulk Audiobook Parse Results ===");
        println!("Total audio files: {total}");
        println!("Total issues: {}", issues.len());
        println!("\nIssue counts:");
        let mut counts: Vec<_> = issue_counts.iter().collect();
        counts.sort_by_key(|(_, c)| std::cmp::Reverse(**c));
        for (category, count) in &counts {
            println!("  {category}: {count}");
        }

        // Print all issues (for subagent analysis)
        println!("\n=== All Issues ===");
        for issue in &issues {
            println!("{issue}");
        }

        // Print sample of parsed results (first file per unique directory)
        println!("\n=== Sample Results (first file per directory) ===");
        let mut seen_dirs: std::collections::HashSet<String> = std::collections::HashSet::new();
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() {
                continue;
            }
            let ext = line.rsplit('.').next().unwrap_or("").to_lowercase();
            if !audio_exts.contains(&ext.as_str()) {
                continue;
            }
            let rel_path = line.strip_prefix(&media_root).unwrap_or(line);

            // Get directory
            let dir = rel_path.rsplit_once('/').map(|(d, _)| d).unwrap_or("");
            if !seen_dirs.insert(dir.to_string()) {
                continue;
            }

            let result = parse_path(rel_path, "audiobooks");
            if let ParsedMedia::Audiobook(ab) = &result.resolved.media {
                println!(
                    "PATH: {}\n  title={:?} author={:?} narrator={:?} series={:?} pos={:?} year={:?} generic={}\n",
                    rel_path,
                    ab.title,
                    ab.author,
                    ab.narrator,
                    ab.series,
                    ab.series_position,
                    ab.year,
                    ab.is_generic_filename
                );
            }
        }
    }
}
