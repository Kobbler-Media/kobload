//! Common parsing utilities shared across media types.

use regex::Regex;
use std::sync::LazyLock;

use super::types::{ParsedMedia, TvParsed};

// =============================================================================
// Regexes
// =============================================================================

/// Year pattern (1900-2099).
pub static YEAR_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:^|[\s.\-\(\[])(?P<year>(?:19|20)\d{2})(?:[\s.\-\)\]]|$)").unwrap()
});

/// Release group pattern: -GroupName at end of title.
pub static RELEASE_GROUP_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"-([A-Za-z0-9]+)(?:\.[a-z]{2,4})?$").unwrap());

/// Quality tokens that should be stripped from titles.
/// Word-bounded to avoid matching inside normal words (e.g. "ts" inside "Limits").
pub static QUALITY_TOKENS: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?ix)\b(?:
        # Resolutions
        4320p|2160p|1080[pi]|720p|576[pi]|480[pi]|4k|uhd|
        # Sources
        remux|bluray|blu-ray|bdrip|brrip|web-dl|webdl|webrip|hdtv|pdtv|dvdrip|dvd|
        cam|camrip|hdcam|telesync|ts|hdts|telecine|tc|screener|scr|r5|
        satellite|satrip|dsr|sdtv|tvrip|vod|ppv|
        # Streaming services
        amzn|amazon|nf|netflix|dsnp|disney|atvp|appletv|hmax|hbo|hulu|pcok|
        peacock|stan|crav|criterion|crunchyroll|funimation|
        # Codecs
        x265|h265|hevc|x264|h264|avc|av1|xvid|divx|
        # Audio
        truehd|dts-hd\.?ma|dts-x|dtsx|atmos|dts|ddp|dd\+|eac3|ac3|aac|flac|mp3|
        # HDR
        dv|dovi|dolby\.?vision|hdr10\+?|hdr|hlg|
        # 3D
        3d|hsbs|sbs|htab|hou|mvc|
        # Release flags
        proper|repack|rerip|real|internal|retail|limited|nfofix|samplefix|
        syncfix|audiofix|dirfix|nuked|subbed|dubbed|hardcoded|hc|korsub|
        # Languages (full names)
        multi|dual|english|french|german|spanish|italian|russian|korean|
        japanese|chinese|hindi|arabic|portuguese|
        dutch|swedish|danish|norwegian|finnish|polish|czech|hungarian|turkish|
        thai|vietnamese|indonesian|malay|tagalog|hebrew|greek|romanian|
        bulgarian|ukrainian|mandarin|cantonese|latino|castellano|flemish|
        # Languages (ISO 639-2/B codes)
        ita|fre|fra|ger|deu|spa|rus|kor|jpn|chn|chi|hin|ara|por|nld|
        swe|dan|nor|fin|pol|ces|cze|hun|tur|tha|vie|ind|msa|tgl|heb|
        ell|ron|bul|ukr|
        # Language scene tags
        truefrench|vostfr|subfrench|fastsub|
        # Common scene tags
        complete|season|s\d{1,2}|ep?\d{1,3}|part\.?\d+|disc\.?\d+|cd\.?\d+
    )\b",
    )
    .unwrap()
});

// -- Folder noise patterns --

static BITRATE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b\d+\s?k(?:bps?)?\b").unwrap());

static DURATION_STAMP_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b\d{2,3}\.\d{2}\.\d{2}\b").unwrap());

static BRACE_TAG_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\{[^}]*\}").unwrap());

static AUDIO_PAREN_TAG_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\(\s*(?:mp3|m4[ab]|flac|ogg|opus|aac|wma)\s*\)").unwrap());

pub static TRAILING_PAREN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\([^)]+\)\s*$").unwrap());

static LEADING_YEAR_DASH_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\d{4}\s*-\s*").unwrap());

/// Capture leading year value (just the digits).
static LEADING_YEAR_CAPTURE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(?P<year>\d{4})\s*-\s*").unwrap());

/// Capture trailing parenthetical content (the text inside parens).
static TRAILING_PAREN_CAPTURE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\((?P<content>[^)]+)\)\s*$").unwrap());

// -- Bracket tag patterns --

static TRAILING_BRACKET_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s*\[[^\]]*\]\s*$").unwrap());

/// Torrent site watermark prefix patterns:
///   "www.SiteName.org - Title"
///   "[www.SiteName.com] - Title"
///   "www.1TamilRockers.org - Title"
///   "【高清剧集网发布 www.BPHDTV.com】Title"
/// Requires `www.` to distinguish from release titles like "Movie.2024 - Subtitle".
static SITE_PREFIX_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^(?:\[?www\.[^\s\[\]]+\.[a-z]{2,4}\]?\s*[-–—]\s*|【[^】]*www\.[^\s】]+[^】]*】)",
    )
    .unwrap()
});

/// Torrent site watermark suffix: bracket-wrapped domain-like tags.
/// Matches "[text.tld]" patterns (e.g. "[EZTVx.to]", "[1TamilRockers.org]").
/// Only matches when the bracket content looks like a domain (contains a dot
/// followed by a 2-4 char TLD). Won't match group names like "[i_c]" or "[Ben The Men]".
static SITE_SUFFIX_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\[[\w.-]+\.[a-z]{2,4}\]").unwrap());

// -- Part suffix patterns --

static PART_SUFFIX_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(?:-Part\d+|\s*\(\d+\))\s*$").unwrap());

// -- Leading number patterns --

static LEADING_NUMBER_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d+[.\s]\s*").unwrap());

// -- Dot-parsed patterns --

static DOT_SEPARATED_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[^/\\]+\.[^/\\]+\.[^/\\]+.*-").unwrap());

static TRAILING_HASH_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[._][0-9a-f]{4,}-\d+$").unwrap());

static BOOK_POSITION_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b(?:Book|Bk|Series)\s+(\d+(?:\.\d+)?)\b").unwrap());
// Matches initials like "R.R." so dots between single uppercase letters are preserved.
#[allow(dead_code)]
static INITIALS_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?-u)\b([A-Z])\.([A-Z])\.?").unwrap());

// -- Generic filename patterns --

static PURE_DIGITS_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\d+$").unwrap());

static TRACK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^(?:track|tr)[\s_.\-]?\d+$").unwrap());

static CHAPTER_DIGITS_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^(?:chapter|ch)[\s_.\-]?\d+$").unwrap());

static DISC_TRACK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^(?:disc|d)\d+[\s_.\-]\d+$").unwrap());

static DIGITS_UNDERSCORE_PREFIX_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\d+_").unwrap());

static LEADING_DIGITS_PREFIX_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[\d\s\-]+").unwrap());

// =============================================================================
// Title Cleaning
// =============================================================================

/// Strip torrent site watermarks from a release name.
/// Handles prefixes like "www.UIndex.org - " and suffixes like "[EZTVx.to]".
pub fn strip_site_watermarks(name: &str) -> String {
    let name = SITE_PREFIX_RE.replace(name, "");
    let name = SITE_SUFFIX_RE.replace(&name, "");
    name.trim().to_string()
}

/// Clean a release name by removing quality tokens, release groups, etc.
/// Returns the cleaned title with proper spacing.
pub fn clean_title(name: &str) -> String {
    // Remove file extension if present
    let name = remove_extension(name);

    // Strip torrent site watermarks before any other processing
    let name = strip_site_watermarks(&name);

    // Remove release group
    let name = RELEASE_GROUP_RE.replace(&name, "");

    // Replace dots/underscores with spaces
    let name = name.replace(['.', '_'], " ");

    // Replace " - " separator (filesystem-safe colon replacement) with space
    let name = name.replace(" - ", " ");

    // Remove quality tokens
    let name = QUALITY_TOKENS.replace_all(&name, " ");

    // Remove content in brackets that looks like quality info
    let name = remove_quality_brackets(&name);

    // Clean up multiple spaces and trim
    let name = normalize_spaces(&name);

    // Trim trailing punctuation (handles "Barry -" → "Barry" from "Barry - S04E05")
    name.trim_end_matches(['-', '–', '—', '.', '_'])
        .trim()
        .to_string()
}

/// A quality/flag token at the very end of a title, preceded by at least one
/// other word.
static TRAILING_QUALITY_TOKEN_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(&format!(r"\s(?:{})\s*$", QUALITY_TOKENS.as_str())).unwrap());

/// Clean the part of a movie release name that precedes the year.
///
/// Unlike `clean_title`, quality/flag tokens are only stripped from the end:
/// before the year they are title words more often than tags —
/// "The.Odyssey.The.Real.Story.2026" must keep "Real", "Real.Steel.2011" must
/// keep "Real". Trailing tags ("Dao.AKA.The.Blade.REPACK.1995") still go.
/// No release-group strip either: the group trails the whole name, so here it
/// would only eat a hyphenated last word ("Spider-Man" → "Spider").
pub fn clean_pre_year_title(name: &str) -> String {
    let name = strip_site_watermarks(name);
    let name = name.replace(['.', '_'], " ");
    let mut name = name.replace(" - ", " ");
    while let Some(start) = TRAILING_QUALITY_TOKEN_RE.find(&name).map(|m| m.start()) {
        name.truncate(start);
    }
    let name = remove_quality_brackets(&name);
    let name = normalize_spaces(&name);
    name.trim_end_matches(['-', '–', '—', '.', '_'])
        .trim()
        .to_string()
}

/// Clean an audiobook title — lighter than `clean_title`, only removes
/// extensions, release groups, and normalizes spacing. Does NOT strip
/// quality tokens (which would false-positive on words like "Greek",
/// "Peacock", "Stan" in actual book titles).
pub fn clean_audiobook_title(name: &str) -> String {
    let name = remove_extension(name);
    let name = RELEASE_GROUP_RE.replace(&name, "");
    let name = name.replace(['.', '_'], " ");
    let name = normalize_spaces(&name);
    name.trim_end_matches(['-', '–', '—', '.', '_'])
        .trim()
        .to_string()
}

/// Remove file extension from a name.
/// Handles compound extensions like `.kepub.epub` by stripping iteratively.
pub fn remove_extension(name: &str) -> String {
    // Common media extensions
    const EXTENSIONS: &[&str] = &[
        "mkv", "mp4", "avi", "wmv", "webm", "m4v", "flv", "mov", // Video
        "mp3", "flac", "m4a", "m4b", "ogg", "opus", "wav", "wma", // Audio
        "epub", "mobi", "azw", "azw3", "pdf", "cbz", "cbr", "kepub", // Ebooks
        "nzb", "torrent", // Meta
        "srt", "sub", "idx", "ass", "ssa", "vtt", // Subtitles
        "nfo", "txt", "sfv", "par2", // Info
        "rar", "zip", "7z", "tar", "gz", // Archives
    ];

    let mut result = name.to_string();
    // Loop to handle compound extensions like .kepub.epub
    loop {
        if let Some(pos) = result.rfind('.') {
            let ext = result[pos + 1..].to_lowercase();
            if EXTENSIONS.contains(&ext.as_str()) {
                result = result[..pos].to_string();
                continue;
            }
        }
        break;
    }
    result
}

/// Brackets containing quality information.
static QUALITY_BRACKETS_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\s*[\[\(][^\[\]\(\)]*(?:1080|720|480|2160|4k|web|blu|hdr|dv|atmos|dts|aac|x26|hevc|av1)[\[\]\(\)]*[\]\)]\s*").unwrap()
});

/// Remove brackets containing quality information.
fn remove_quality_brackets(name: &str) -> String {
    QUALITY_BRACKETS_RE.replace_all(name, " ").to_string()
}

/// Normalize whitespace - collapse multiple spaces, trim.
pub fn normalize_spaces(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Extract year from a string, returns (year, position).
pub fn extract_year(s: &str) -> Option<(u32, usize)> {
    YEAR_RE.captures(s).and_then(|caps| {
        let m = caps.name("year")?;
        let year: u32 = m.as_str().parse().ok()?;
        Some((year, m.start()))
    })
}

// =============================================================================
// Unknown Media Parsing
// =============================================================================

/// File extension categories for routing auto-detection.
const VIDEO_EXTENSIONS: &[&str] = &[
    "mkv", "mp4", "avi", "wmv", "flv", "mov", "webm", "ts", "m4v", "m2ts",
];
const EBOOK_EXTENSIONS: &[&str] = &["epub", "mobi", "azw", "azw3", "pdf", "cbz", "cbr", "fb2"];
const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "flac", "m4a", "ogg", "opus", "wav", "aac", "wma", "m4b",
];

/// Extract the file extension (lowercased) from a name.
fn extract_extension(name: &str) -> Option<String> {
    let dot_pos = name.rfind('.')?;
    let ext = &name[dot_pos + 1..];
    if ext.is_empty() {
        return None;
    }
    Some(ext.to_lowercase())
}

/// Pick the better of two parse results, using confidence as primary and
/// TV season structure as tie-breaker.
fn pick_best_video(
    tv: (String, ParsedMedia, f64),
    movie: (String, ParsedMedia, f64),
) -> (String, ParsedMedia, f64) {
    let (_, ref tv_media, tv_conf) = tv;
    let (_, _, movie_conf) = movie;

    if tv_conf > movie_conf {
        return tv;
    }
    if movie_conf > tv_conf {
        return movie;
    }
    // Tie-break: TV wins if it found season/episode structure
    if let ParsedMedia::Tv(TvParsed {
        season: Some(_), ..
    }) = tv_media
    {
        return tv;
    }
    movie
}

/// Parse an unknown/auto media type using extension-based routing.
///
/// Tries the appropriate parsers based on file extension and picks the
/// best result by confidence score.
pub fn parse_unknown(name: &str) -> (String, ParsedMedia, f64) {
    let ext = extract_extension(name);
    let ext_str = ext.as_deref().unwrap_or("");

    if EBOOK_EXTENSIONS.contains(&ext_str) {
        return super::book::parse_book(name);
    }

    if AUDIO_EXTENSIONS.contains(&ext_str) {
        let audiobook = super::audiobook::parse_audiobook(name);
        let music = super::music::parse_music(name);
        return if audiobook.2 >= music.2 {
            audiobook
        } else {
            music
        };
    }

    // Video extensions or unknown/no extension: try TV + movie
    let is_video_ext = VIDEO_EXTENSIONS.contains(&ext_str);
    if is_video_ext || ext_str.is_empty() || !known_extension(ext_str) {
        let tv = super::tv::parse_tv(name);
        let movie = super::movie::parse_movie(name);

        // Sport detection: an untyped sport grab (e.g. a manual download with no
        // media_kind) would otherwise parse as a movie titled after the league
        // ("MotoGP 2026"), dropping location/round/session — which makes kobbler
        // mis-place the event. Prefer the sport parse only when it found a round
        // or a session_type AND the TV parser saw no season/episode structure.
        // The round/session tokens (R03, Round03, 2026x03, Race/Sprint/FP1, ...)
        // don't occur in movie/TV titles, and the TV guard keeps a real episode
        // release (S01E03) from being misread as a sport. A bare
        // "League.Year.Location" has neither token, so it stays with TV/movie.
        let tv_has_episode_structure = matches!(
            tv.1,
            ParsedMedia::Tv(TvParsed {
                season: Some(_),
                ..
            })
        );
        if !tv_has_episode_structure {
            let sport = super::sport::parse_sport(name);
            if let ParsedMedia::Sport(ref s) = sport.1
                && (s.round.is_some() || s.session_type.is_some())
            {
                return sport;
            }
        }

        let best = pick_best_video(tv, movie);

        // For non-video files, require strong signals (year, season/episode).
        // Parsers return ~0.7 baseline even for bare titles, so we need > 0.8
        // to indicate actual structural match.
        if !is_video_ext && best.2 < 0.85 {
            let title = clean_title(name);
            return (
                title.clone(),
                ParsedMedia::Unknown {
                    cleaned_title: title,
                },
                best.2,
            );
        }

        return best;
    }

    // Truly unknown extension — just clean the title
    let title = clean_title(name);
    (
        title.clone(),
        ParsedMedia::Unknown {
            cleaned_title: title,
        },
        0.5,
    )
}

/// Check if an extension belongs to any known category.
fn known_extension(ext: &str) -> bool {
    VIDEO_EXTENSIONS.contains(&ext)
        || EBOOK_EXTENSIONS.contains(&ext)
        || AUDIO_EXTENSIONS.contains(&ext)
}

// =============================================================================
// Folder / Filename Utilities
// =============================================================================

/// Strip scene-release noise from directory names (bitrates, duration stamps,
/// format tags, trailing parenthesized content, leading year-dash).
pub fn strip_folder_noise(name: &str) -> String {
    let s = BITRATE_RE.replace_all(name, " ");
    let s = DURATION_STAMP_RE.replace_all(&s, " ");
    let s = BRACE_TAG_RE.replace_all(&s, " ");
    let s = AUDIO_PAREN_TAG_RE.replace_all(&s, " ");
    let s = TRAILING_PAREN_RE.replace(&s, "");
    let s = LEADING_YEAR_DASH_RE.replace(&s, "");
    normalize_spaces(&s)
}

/// Metadata extracted from a folder name before noise stripping.
#[derive(Debug, Clone, Default)]
pub struct FolderMetadata {
    /// Name after stripping noise (same as `strip_folder_noise` output).
    pub clean_name: String,
    /// Year from leading "YYYY - " pattern.
    pub year: Option<u32>,
    /// Narrator from trailing parenthetical (if it looks like a person name).
    pub narrator: Option<String>,
}

/// Words in trailing parens that are noise, not narrator names.
const PAREN_NOISE_WORDS: &[&str] = &[
    "unabridged",
    "abridged",
    "mp3",
    "m4b",
    "m4a",
    "flac",
    "ogg",
    "opus",
    "aac",
    "wma",
    "retail",
    "ch",
    "complete",
    "audiobook",
    "audio",
    "book",
    "series",
    "edition",
    "version",
    "remastered",
    "hq",
    "lq",
    "vbr",
    "cbr",
];

/// Extract metadata (year, narrator) from a folder name, then strip noise.
///
/// Unlike `strip_folder_noise` which discards everything, this captures
/// the year and narrator before stripping.
pub fn extract_folder_metadata(name: &str) -> FolderMetadata {
    let mut meta = FolderMetadata::default();

    // Extract year from leading "YYYY - "
    if let Some(caps) = LEADING_YEAR_CAPTURE_RE.captures(name)
        && let Some(y) = caps.name("year")
        && let Ok(year) = y.as_str().parse::<u32>()
        && (1900..2100).contains(&year)
    {
        meta.year = Some(year);
    }

    // Before stripping trailing parens, check if the content looks like a narrator.
    // We need to check the name after stripping audio format parens and other noise
    // but before stripping the final trailing paren.
    let after_bitrate = BITRATE_RE.replace_all(name, " ");
    let after_duration = DURATION_STAMP_RE.replace_all(&after_bitrate, " ");
    let after_braces = BRACE_TAG_RE.replace_all(&after_duration, " ");
    let after_audio_parens = AUDIO_PAREN_TAG_RE.replace_all(&after_braces, " ");
    let trimmed = normalize_spaces(&after_audio_parens);

    // Now check the trailing paren on the partially-cleaned string
    if let Some(caps) = TRAILING_PAREN_CAPTURE_RE.captures(&trimmed)
        && let Some(content) = caps.name("content")
    {
        let text = content.as_str().trim();
        if looks_like_narrator(text) {
            meta.narrator = Some(text.to_string());
        }
    }

    meta.clean_name = strip_folder_noise(name);
    meta
}

/// Check if parenthetical content looks like a narrator name.
///
/// A narrator is 1-3 capitalized words, not a known noise word.
fn looks_like_narrator(text: &str) -> bool {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.is_empty() || words.len() > 3 {
        return false;
    }

    // Reject if any word is a known noise word
    for word in &words {
        if PAREN_NOISE_WORDS
            .iter()
            .any(|n| n.eq_ignore_ascii_case(word))
        {
            return false;
        }
    }

    // At least the first word should start with an uppercase letter
    words[0]
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_uppercase())
}

/// Strip bracketed metadata tags from the end of a name
/// (e.g. `[B0F14RFHS6]`, `[EC-3]`).
pub fn strip_bracket_tags(name: &str) -> String {
    let mut s = name.to_string();
    // Repeatedly strip trailing bracket tags (there may be several).
    while TRAILING_BRACKET_RE.is_match(&s) {
        s = TRAILING_BRACKET_RE.replace(&s, "").to_string();
    }
    s.trim().to_string()
}

/// Result of parsing a dot-separated scene-release directory name.
#[derive(Debug, Clone, Default)]
pub struct DotParsed {
    pub author: Option<String>,
    pub series: Option<String>,
    pub title: Option<String>,
    pub series_position: Option<String>,
    pub year: Option<u32>,
}

/// Parse a scene-release directory name that uses dots as spaces and hyphens
/// to delimit sections (author, series/title, group).
///
/// Returns `None` if the name does not look dot-separated.
pub fn parse_dot_separated_dirname(name: &str) -> Option<DotParsed> {
    // Must have 3+ dots and 1+ hyphens.
    if name.matches('.').count() < 3 || !name.contains('-') {
        return None;
    }
    // Extra gate: check overall shape.
    if !DOT_SEPARATED_RE.is_match(name) {
        return None;
    }

    let mut working = name.to_string();

    // Strip trailing hex-hash + digits suffix (e.g. ".9af496-17").
    working = TRAILING_HASH_RE.replace(&working, "").to_string();

    // Strip trailing group name after last hyphen if it looks like a scene
    // group: short, alphanumeric, or known obfuscation markers, or starts
    // with '(' (truncated paren).
    if let Some(last_dash) = working.rfind('-') {
        let candidate = &working[last_dash + 1..];
        let is_group = candidate.len() <= 20
            && (candidate.chars().all(|c| c.is_alphanumeric())
                || candidate.contains("Obfuscated")
                || candidate.starts_with('('));
        if is_group {
            working = working[..last_dash].to_string();
        }
    }

    // Split on hyphens — first segment is author, rest is title/series area.
    let segments: Vec<&str> = working.splitn(2, '-').collect();
    let raw_author = segments[0];
    let rest = if segments.len() > 1 { segments[1] } else { "" };

    // Replace dots with spaces, but preserve initials (e.g. "R.R." → "R.R.").
    let author_str = dots_to_spaces_preserve_initials(raw_author);
    let author = title_case(&normalize_spaces(&author_str));

    let mut parsed = DotParsed {
        author: Some(author),
        ..Default::default()
    };

    // Extract year from original name.
    if let Some((year, _)) = extract_year(name) {
        parsed.year = Some(year);
    }

    if !rest.is_empty() {
        // Convert dots to spaces first so position regexes can match.
        let rest_spaced = dots_to_spaces_preserve_initials(rest);

        // Look for series position markers.
        if let Some(caps) = BOOK_POSITION_RE.captures(&rest_spaced) {
            parsed.series_position = Some(caps[1].to_string());
        }

        // Try to split rest further on hyphens for series vs. title.
        // Pattern: Series.Name.Bk.N-Title.Words
        let rest_parts: Vec<&str> = rest.splitn(2, '-').collect();
        if rest_parts.len() == 2 {
            let part1 = dots_to_spaces_preserve_initials(rest_parts[0]);
            let part2 = dots_to_spaces_preserve_initials(rest_parts[1]);
            let part1_clean = strip_position_markers(&normalize_spaces(&part1));
            let part2_clean = normalize_spaces(&part2);
            parsed.series = Some(part1_clean);
            parsed.title = Some(part2_clean);
        } else {
            let clean = strip_position_markers(&normalize_spaces(&rest_spaced));
            // If we found a position, treat the text as a series name.
            if parsed.series_position.is_some() {
                parsed.series = Some(clean);
            } else {
                parsed.title = Some(clean);
            }
        }
    }

    Some(parsed)
}

/// Replace dots with spaces but keep initials like "R.R." intact.
///
/// An initial is a single uppercase letter preceded by a word boundary (start
/// of string, dot, or space) followed by a dot and another single-letter
/// initial.
fn dots_to_spaces_preserve_initials(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let len = chars.len();
    let mut result = String::with_capacity(len);
    let mut i = 0;

    while i < len {
        // Check for initial: single uppercase letter at a word boundary,
        // followed by dot, then another uppercase letter also at a boundary.
        let at_boundary = i == 0 || chars[i - 1] == '.' || chars[i - 1] == ' ';
        let is_single_letter =
            at_boundary && chars[i].is_ascii_uppercase() && (i + 1 >= len || chars[i + 1] == '.');

        if is_single_letter
            && i + 2 < len
            && chars[i + 1] == '.'
            && chars[i + 2].is_ascii_uppercase()
            // The next char must also be a single letter (followed by dot or end).
            && (i + 3 >= len || chars[i + 3] == '.' || !chars[i + 3].is_ascii_alphabetic())
        {
            // Consume the full initials run (e.g. "R.R." or "J.R.R.").
            result.push(chars[i]);
            result.push('.');
            i += 2;
            while i < len
                && chars[i].is_ascii_uppercase()
                && (i + 1 >= len || chars[i + 1] == '.' || !chars[i + 1].is_ascii_alphabetic())
            {
                result.push(chars[i]);
                result.push('.');
                i += 1;
                if i < len && chars[i] == '.' {
                    i += 1;
                }
            }
            // Insert a space separator if more content follows.
            if i < len && chars[i] != '.' {
                result.push(' ');
            }
        } else if chars[i] == '.' {
            result.push(' ');
            i += 1;
        } else {
            result.push(chars[i]);
            i += 1;
        }
    }
    result
}

/// Strip "Book N", "Bk N", "Series N" markers from a string.
fn strip_position_markers(s: &str) -> String {
    let result = BOOK_POSITION_RE.replace(s, "");
    normalize_spaces(result.trim())
}

/// Title-case a string (capitalize first letter of each word, lowercase rest).
fn title_case(s: &str) -> String {
    s.split_whitespace()
        .map(|word| {
            // Don't touch words that look like initials (e.g. "R.R.").
            if word.len() <= 4 && word.contains('.') {
                return word.to_string();
            }
            let mut chars = word.chars();
            match chars.next() {
                None => String::new(),
                Some(c) => {
                    let upper: String = c.to_uppercase().collect();
                    let lower: String = chars.as_str().to_lowercase();
                    format!("{upper}{lower}")
                }
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Detect filenames that are too generic to serve as a title (pure track
/// numbers, chapter markers without descriptive text, etc.).
///
/// `name` should be the file stem (no extension).
pub fn is_generic_audiobook_filename(name: &str) -> bool {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return true;
    }
    if PURE_DIGITS_RE.is_match(trimmed) {
        return true;
    }
    if TRACK_RE.is_match(trimmed) {
        return true;
    }
    if CHAPTER_DIGITS_RE.is_match(trimmed) {
        return true;
    }
    if DISC_TRACK_RE.is_match(trimmed) {
        return true;
    }
    // Starts with digits + underscore: mangled filename (e.g. "0001_13storeyTreehouse").
    if DIGITS_UNDERSCORE_PREFIX_RE.is_match(trimmed) {
        return true;
    }
    // Starts with digits + space/dash — check if remainder is meaningful.
    let stripped = LEADING_DIGITS_PREFIX_RE.replace(trimmed, "");
    stripped.is_empty() || stripped.len() <= 2
}

/// Strip `-Part\d+` or `(\d+)` suffixes from a name.
pub fn strip_part_suffix(name: &str) -> String {
    PART_SUFFIX_RE.replace(name, "").trim().to_string()
}

/// Strip a leading number prefix like "01 " or "01. " from a directory name.
pub fn strip_leading_number(name: &str) -> String {
    LEADING_NUMBER_RE.replace(name, "").to_string()
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clean_title_basic() {
        assert_eq!(
            clean_title("Movie.Name.2024.1080p.BluRay.x264-GROUP"),
            "Movie Name 2024"
        );
    }

    #[test]
    fn test_clean_title_with_dots() {
        // Note: standalone "WEB" isn't stripped (only "WEB-DL", "WEBRIP" etc.)
        // but "720p" and "-DL" are stripped, leaving "WEB"
        let result = clean_title("The.Movie.Name.2024.720p.WEB-DL");
        assert!(result.starts_with("The Movie Name 2024"));
    }

    #[test]
    fn test_remove_extension() {
        assert_eq!(remove_extension("movie.mkv"), "movie");
        assert_eq!(remove_extension("movie.mp4"), "movie");
        assert_eq!(remove_extension("movie.name.mkv"), "movie.name");
        assert_eq!(remove_extension("movie.with.dots"), "movie.with.dots"); // Not an extension
    }

    #[test]
    fn test_remove_compound_extension() {
        // .kepub.epub is a compound extension used by Kobo readers
        assert_eq!(remove_extension("Book Title.kepub.epub"), "Book Title");
        assert_eq!(
            remove_extension("Author - Title.kepub.epub"),
            "Author - Title"
        );
    }

    #[test]
    fn test_quality_tokens_dont_match_inside_words() {
        // "ts" is a quality token (transport stream) but shouldn't match inside "Limits"
        assert_eq!(
            clean_title("Profiles of the Future an Inquiry Into the Limits of the Possible"),
            "Profiles of the Future an Inquiry Into the Limits of the Possible"
        );
        // "dts" shouldn't eat the end of "Credits"
        assert!(clean_title("End Credits").contains("Credits"));
        // But standalone quality tokens should still be stripped
        assert_eq!(clean_title("Movie Name TS"), "Movie Name");
        assert_eq!(clean_title("Movie.Name.720p.BluRay"), "Movie Name");
    }

    #[test]
    fn test_extract_year() {
        assert_eq!(extract_year("Movie 2024"), Some((2024, 6)));
        assert_eq!(extract_year("Movie.2024.1080p"), Some((2024, 6)));
        // Position is index of the first digit of the year within the matched group
        let result = extract_year("Movie (2024)");
        assert!(matches!(result, Some((2024, pos)) if pos == 7 || pos == 8));
        let result2 = extract_year("Movie [2024]");
        assert!(matches!(result2, Some((2024, pos)) if pos == 7 || pos == 8));
        assert_eq!(extract_year("No year here"), None);
        assert_eq!(extract_year("1080p Movie"), None); // 1080 isn't a year pattern
    }

    #[test]
    fn test_clean_title_trailing_dash() {
        // TV filenames like "Barry - S04E05" leave "Barry -" after extracting SxxExx
        assert_eq!(clean_title("Barry - "), "Barry");
        assert_eq!(clean_title("Shrinking -"), "Shrinking");
        assert_eq!(clean_title("Show.Name.-"), "Show Name");
    }

    #[test]
    fn test_clean_pre_year_title_keeps_mid_title_flag_words() {
        // "Real" is a release flag token, but mid-title it's part of the name.
        assert_eq!(
            clean_pre_year_title("The.Odyssey.The.Real.Story"),
            "The Odyssey The Real Story"
        );
        assert_eq!(clean_pre_year_title("Real.Steel"), "Real Steel");
        assert_eq!(clean_pre_year_title("The.Italian.Job"), "The Italian Job");
    }

    #[test]
    fn test_clean_pre_year_title_strips_trailing_tags() {
        assert_eq!(
            clean_pre_year_title("Dao.AKA.The.Blade.REPACK"),
            "Dao AKA The Blade"
        );
        assert_eq!(clean_pre_year_title("Avatar.3D"), "Avatar");
        assert_eq!(clean_pre_year_title("Movie.Name.PROPER.3D"), "Movie Name");
        // Never strips the whole title.
        assert_eq!(clean_pre_year_title("Limited"), "Limited");
    }

    #[test]
    fn test_clean_pre_year_title_keeps_hyphenated_last_word() {
        assert_eq!(clean_pre_year_title("Spider-Man"), "Spider-Man");
    }

    #[test]
    fn test_normalize_spaces() {
        assert_eq!(normalize_spaces("  hello   world  "), "hello world");
        assert_eq!(normalize_spaces("single"), "single");
    }

    // =========================================================================
    // parse_unknown auto-detection tests
    // =========================================================================

    #[test]
    fn test_unknown_detects_tv() {
        let (_, media, conf) = parse_unknown("Show.S01E05.720p.mkv");
        assert!(
            matches!(media, ParsedMedia::Tv(_)),
            "Expected Tv, got {:?}",
            media
        );
        assert!(conf > 0.5);
    }

    #[test]
    fn test_unknown_detects_movie() {
        let (_, media, conf) = parse_unknown("Inception.2010.1080p.mkv");
        assert!(
            matches!(media, ParsedMedia::Movie(_)),
            "Expected Movie, got {:?}",
            media
        );
        assert!(conf > 0.5);
    }

    #[test]
    fn test_unknown_detects_book() {
        let (_, media, _) = parse_unknown("Book Title.epub");
        assert!(
            matches!(media, ParsedMedia::Book(_)),
            "Expected Book, got {:?}",
            media
        );
    }

    #[test]
    fn test_unknown_prefers_tv_over_movie() {
        let (_, media, _) = parse_unknown("Barry.1x01.mkv");
        assert!(
            matches!(media, ParsedMedia::Tv(_)),
            "Expected Tv (has episode structure), got {:?}",
            media
        );
    }

    #[test]
    fn test_unknown_ambiguous_video() {
        let (_, media, conf) = parse_unknown("SomeFile.mkv");
        // Should still return something (TV or Movie), not Unknown
        assert!(
            !matches!(media, ParsedMedia::Unknown { .. }),
            "Expected Tv or Movie for video file, got {:?}",
            media
        );
        // Confidence should be lower for ambiguous input
        assert!(conf <= 0.8);
    }

    #[test]
    fn test_unknown_no_extension() {
        let (_, media, _) = parse_unknown("SomeFile");
        // No signals at all — should fall back to Unknown
        assert!(
            matches!(media, ParsedMedia::Unknown { .. }),
            "Expected Unknown for bare name, got {:?}",
            media
        );
    }

    #[test]
    fn test_unknown_detects_sport_with_round() {
        // A round token (R03) is a sport-only signal — must not parse as a movie
        // titled "MotoGP". Mirrors the manual-grab path where media_kind is unset.
        let (_, media, _) = parse_unknown("MotoGP.2026.R03.USA.Race.1080p.WEB.h264-BILLIE");
        match media {
            ParsedMedia::Sport(s) => {
                assert_eq!(s.location.as_deref(), Some("USA"));
                assert_eq!(s.round, Some(3));
            }
            other => panic!("Expected Sport, got {other:?}"),
        }
    }

    #[test]
    fn test_unknown_detects_sport_with_session() {
        // Session token (Qualifying) is sport-only even without an explicit round.
        let (_, media, _) = parse_unknown("Moto2.2026.Czechia.Qualifying.1080p.WEB.h264-VERUM");
        match media {
            ParsedMedia::Sport(s) => {
                assert_eq!(s.location.as_deref(), Some("Czechia"));
                assert!(s.session_type.is_some());
            }
            other => panic!("Expected Sport, got {other:?}"),
        }
    }

    #[test]
    fn test_unknown_bare_league_year_location_not_sport() {
        // No round, no session: structurally indistinguishable from a movie, so
        // the sport guard must NOT fire. (The systematic fix for this case is
        // threading media_kind through grabs, not heuristics here.)
        let (_, media, _) = parse_unknown("MotoGP.2026.Czechia.1080p.WEB.h264-BILLIE");
        assert!(
            !matches!(media, ParsedMedia::Sport(_)),
            "Bare league.year.location must not be force-detected as Sport, got {media:?}"
        );
    }

    #[test]
    fn test_unknown_movie_not_misdetected_as_sport() {
        // An ordinary movie with a location-like word must stay a Movie.
        let (_, media, _) = parse_unknown("Casablanca.1942.1080p.BluRay.x264-GROUP");
        assert!(
            matches!(media, ParsedMedia::Movie(_)),
            "Expected Movie, got {media:?}"
        );
    }

    // =========================================================================
    // strip_folder_noise
    // =========================================================================

    #[test]
    fn test_strip_folder_noise_bitrate() {
        assert_eq!(strip_folder_noise("Title 64k"), "Title");
        assert_eq!(strip_folder_noise("Title 128kbps"), "Title");
        assert_eq!(strip_folder_noise("Title 320 kbps"), "Title");
    }

    #[test]
    fn test_strip_folder_noise_duration() {
        assert_eq!(strip_folder_noise("Title 04.10.40"), "Title");
    }

    #[test]
    fn test_strip_folder_noise_brace_tags() {
        assert_eq!(strip_folder_noise("Title {120mb}"), "Title");
    }

    #[test]
    fn test_strip_folder_noise_audio_format_parens() {
        assert_eq!(strip_folder_noise("Title (mp3)"), "Title");
        assert_eq!(strip_folder_noise("Title (M4B)"), "Title");
        assert_eq!(strip_folder_noise("Title (FLAC)"), "Title");
    }

    #[test]
    fn test_strip_folder_noise_trailing_parens() {
        assert_eq!(strip_folder_noise("Title (Narrator Name)"), "Title");
    }

    #[test]
    fn test_strip_folder_noise_leading_year_dash() {
        assert_eq!(strip_folder_noise("1992 - Title"), "Title");
    }

    #[test]
    fn test_strip_folder_noise_combined() {
        assert_eq!(
            strip_folder_noise("1992 - Great Book 128kbps {120mb} (mp3) (John Smith)"),
            "Great Book"
        );
    }

    // =========================================================================
    // strip_bracket_tags
    // =========================================================================

    #[test]
    fn test_strip_bracket_tags_asin() {
        assert_eq!(strip_bracket_tags("Title [B0F14RFHS6]"), "Title");
    }

    #[test]
    fn test_strip_bracket_tags_multiple() {
        assert_eq!(strip_bracket_tags("Title [EC-3] [B0F14RFHS6]"), "Title");
    }

    #[test]
    fn test_strip_bracket_tags_no_tags() {
        assert_eq!(strip_bracket_tags("Just A Title"), "Just A Title");
    }

    // =========================================================================
    // parse_dot_separated_dirname
    // =========================================================================

    #[test]
    fn test_dot_parsed_jeff_kinney() {
        let result =
            parse_dot_separated_dirname("JEFF.KINNEY-DIARY.OF.A.WIMPY.KID.SERIES.1.9af496-17")
                .unwrap();
        assert_eq!(result.author.as_deref(), Some("Jeff Kinney"));
        assert_eq!(result.series_position.as_deref(), Some("1"));
    }

    #[test]
    fn test_dot_parsed_george_martin() {
        let result = parse_dot_separated_dirname(
            "George.R.R.Martin-A.Song.of.Ice.and.Fire.Bk.4-A.Feast.for.Crows-NMR",
        )
        .unwrap();
        assert_eq!(result.author.as_deref(), Some("George R.R. Martin"));
        assert_eq!(result.title.as_deref(), Some("A Feast for Crows"));
        assert_eq!(result.series_position.as_deref(), Some("4"));
    }

    #[test]
    fn test_dot_parsed_not_dotted() {
        assert!(parse_dot_separated_dirname("Normal Title").is_none());
    }

    // =========================================================================
    // is_generic_audiobook_filename
    // =========================================================================

    #[test]
    fn test_generic_pure_digits() {
        assert!(is_generic_audiobook_filename("01"));
        assert!(is_generic_audiobook_filename("0001"));
    }

    #[test]
    fn test_generic_track() {
        assert!(is_generic_audiobook_filename("track01"));
        assert!(is_generic_audiobook_filename("Track 05"));
        assert!(is_generic_audiobook_filename("tr-01"));
    }

    #[test]
    fn test_generic_chapter_digits_only() {
        assert!(is_generic_audiobook_filename("Chapter 09"));
        assert!(is_generic_audiobook_filename("ch01"));
    }

    #[test]
    fn test_generic_disc_track() {
        assert!(is_generic_audiobook_filename("d1-05"));
        assert!(is_generic_audiobook_filename("disc2 03"));
    }

    #[test]
    fn test_generic_numbered_mangled() {
        assert!(is_generic_audiobook_filename("0001_13storeyTreehouse"));
    }

    #[test]
    fn test_not_generic_descriptive() {
        assert!(!is_generic_audiobook_filename("01 Dad Is Fat"));
        assert!(!is_generic_audiobook_filename(
            "Diary of a Wimpy Kid-Part01"
        ));
    }

    // =========================================================================
    // strip_part_suffix
    // =========================================================================

    #[test]
    fn test_strip_part_suffix() {
        assert_eq!(strip_part_suffix("Title-Part08"), "Title");
        assert_eq!(strip_part_suffix("Title (1)"), "Title");
        assert_eq!(strip_part_suffix("Title-Part1"), "Title");
        assert_eq!(strip_part_suffix("No Suffix Here"), "No Suffix Here");
    }

    // =========================================================================
    // strip_leading_number
    // =========================================================================

    #[test]
    fn test_strip_leading_number() {
        assert_eq!(strip_leading_number("01 Title"), "Title");
        assert_eq!(strip_leading_number("01. Title"), "Title");
        assert_eq!(strip_leading_number("3 Title"), "Title");
        assert_eq!(strip_leading_number("Title"), "Title");
    }

    // =========================================================================
    // remove_extension m4b support
    // =========================================================================

    #[test]
    fn test_remove_extension_m4b() {
        assert_eq!(remove_extension("audiobook.m4b"), "audiobook");
    }

    // =========================================================================
    // extract_folder_metadata
    // =========================================================================

    #[test]
    fn test_extract_folder_metadata_year_narrator() {
        let meta =
            extract_folder_metadata("1992 - The Thief of Always (Sandvold) 64k 04.10.40 {120mb}");
        assert_eq!(meta.year, Some(1992));
        assert_eq!(meta.narrator.as_deref(), Some("Sandvold"));
        assert_eq!(meta.clean_name, "The Thief of Always");
    }

    #[test]
    fn test_extract_folder_metadata_no_narrator_noise() {
        let meta = extract_folder_metadata("Title (mp3)");
        assert!(meta.narrator.is_none(), "mp3 is noise, not narrator");
        assert_eq!(meta.clean_name, "Title");
    }

    #[test]
    fn test_extract_folder_metadata_no_narrator_unabridged() {
        let meta = extract_folder_metadata("Title (Unabridged)");
        assert!(meta.narrator.is_none(), "Unabridged is noise");
    }

    #[test]
    fn test_extract_folder_metadata_multi_word_narrator() {
        let meta = extract_folder_metadata("Title (John Smith)");
        assert_eq!(meta.narrator.as_deref(), Some("John Smith"));
    }

    #[test]
    fn test_extract_folder_metadata_no_year() {
        let meta = extract_folder_metadata("Just a Title (Smith)");
        assert!(meta.year.is_none());
        assert_eq!(meta.narrator.as_deref(), Some("Smith"));
        assert_eq!(meta.clean_name, "Just a Title");
    }
}
