//! TV show episode and season parsing.
//!
//! Handles all common TV release naming conventions:
//! - Standard: `Show.S01E05.Title.1080p`
//! - Multi-episode: `Show.S01E05-E07`, `Show.S01E05E06E07`
//! - Alternative: `Show.1x05.Title`
//! - Season pack: `Show.S01.1080p`
//! - Daily shows: `Show.2024.01.15.Title`
//! - Anime absolute: `[Group] Show - 123`
//! - Miniseries: `Show.Part.1.1080p`
//! - Specials: `Show.S00E05.Christmas.Special`

use regex::Regex;
use std::sync::LazyLock;

use super::common::{clean_title, extract_year, normalize_spaces};
use super::types::{ParsedMedia, SpecialType, TvParsed};

// =============================================================================
// Regexes
// =============================================================================

/// Standard season/episode: S01E05, s01e05
static STANDARD_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)S(?P<season>\d{1,2})E(?P<episode>\d{1,3})").unwrap());

/// Multi-episode range: S01E05-E07, S01E05-07
static MULTI_EP_RANGE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)S(?P<season>\d{1,2})E(?P<start>\d{1,3})[-–]E?(?P<end>\d{1,3})").unwrap()
});

/// Multi-episode consecutive: S01E05E06E07
static MULTI_EP_CONSEC_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)S(?P<season>\d{1,2})(?P<eps>(?:E\d{1,3})+)").unwrap());

/// Alternative multi-episode: 3x12-13 (range), 3x12-13-14 (explicit list)
static ALT_MULTI_EP_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?:^|[\s.\-\[\(])(?P<season>\d{1,2})x(?P<eps>\d{1,3}(?:[-–]\d{1,3})+)(?:[\s.\-\]\),]|$)",
    )
    .unwrap()
});

/// Alternative format: 1x05, 01x05
static ALT_FORMAT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:^|[\s.\-\[\(])(?P<season>\d{1,2})x(?P<episode>\d{1,3})(?:[\s.\-\]\)]|$)")
        .unwrap()
});

/// Season pack: S01, Season 01, Season.1
/// Note: We match S followed by digits and then a delimiter that is NOT 'E' (to avoid matching S01E05)
/// Since regex crate doesn't support look-ahead, we explicitly match the delimiter character classes
static SEASON_PACK_RE: LazyLock<Regex> = LazyLock::new(|| {
    // Match S##. or S##- or S## (end) or Season##, but NOT S##E
    Regex::new(
        r"(?i)(?:S(?P<s1>\d{1,2})(?:[\s.\-\]\)]|$)|Season\.?\s*(?P<s2>\d{1,2})(?:[\s.\-\]\)]|$))",
    )
    .unwrap()
});

/// Daily show: 2024.01.15, 2024-01-15
static DAILY_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?P<year>20\d{2})[\.\-](?P<month>\d{2})[\.\-](?P<day>\d{2})").unwrap()
});

/// Anime absolute episode: - 123, - 0123, Episode 123
static ANIME_ABSOLUTE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:^|[\s.\-])(?:Episode\s*)?(?P<ep>\d{2,4})(?:v\d)?(?:[\s.\-\[\]]|$)").unwrap()
});

/// Anime with brackets: [Group] Show - 123 [1080p]
static ANIME_BRACKET_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^\[(?P<group>[^\]]+)\]\s*(?P<title>.+?)\s*-\s*(?P<ep>\d{2,4})").unwrap()
});

/// Anime OVA/ONA with brackets: [Group] Show - OVA [1080p]
static ANIME_OVA_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)^\[(?P<group>[^\]]+)\]\s*(?P<title>.+?)\s*-\s*(?P<type>OVA|ONA)(?:\s*(?P<num>\d+))?",
    )
    .unwrap()
});

/// Miniseries part: Part 1, Part.1, Part One
static PART_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)Part\.?\s*(?P<num>\d+|One|Two|Three|Four|Five|Six|Seven|Eight|Nine|Ten)")
        .unwrap()
});

/// Episode in various formats for extracting from text
static EP_EXTRACT_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)E(\d{1,3})").unwrap());

// =============================================================================
// Parsing
// =============================================================================

/// Parse a TV release name.
pub fn parse_tv(name: &str) -> (String, ParsedMedia, f64) {
    // Try anime OVA/ONA format first (most specific)
    if let Some(result) = try_parse_anime_ova(name) {
        return result;
    }

    // Try anime bracket format (most specific)
    if let Some(result) = try_parse_anime_bracket(name) {
        return result;
    }

    // Try multi-episode patterns BEFORE standard (more specific)
    // Multi-episode range: S01E05-E07, S01E05-07
    if let Some(result) = try_parse_multi_ep_range(name) {
        return result;
    }

    // Multi-episode consecutive: S01E05E06E07
    if let Some(result) = try_parse_multi_ep_consec(name) {
        return result;
    }

    // Try standard S01E05 format (single episode)
    if let Some(result) = try_parse_standard(name) {
        return result;
    }

    // Try alternative multi-episode: 3x12-13, 3x12-13-14
    if let Some(result) = try_parse_alt_multi_ep(name) {
        return result;
    }

    // Try alternative 1x05 format
    if let Some(result) = try_parse_alt_format(name) {
        return result;
    }

    // Try daily show format
    if let Some(result) = try_parse_daily(name) {
        return result;
    }

    // Try season pack
    if let Some(result) = try_parse_season_pack(name) {
        return result;
    }

    // Try miniseries part
    if let Some(result) = try_parse_miniseries(name) {
        return result;
    }

    // Try anime absolute numbering (less specific, do last)
    if let Some(result) = try_parse_anime_absolute(name) {
        return result;
    }

    // Fallback: just clean the title
    let title = clean_title(name);
    let year = extract_year(&title).map(|(y, _)| y);

    (
        title.clone(),
        ParsedMedia::Tv(TvParsed {
            show_name: title,
            year,
            ..Default::default()
        }),
        0.3,
    )
}

/// Try parsing standard S01E05 format.
fn try_parse_standard(name: &str) -> Option<(String, ParsedMedia, f64)> {
    let caps = STANDARD_RE.captures(name)?;

    let season: u32 = caps.name("season")?.as_str().parse().ok()?;
    let episode: u32 = caps.name("episode")?.as_str().parse().ok()?;

    // Extract title (everything before the SxxExx pattern)
    let match_start = caps.get(0)?.start();
    let title_part = &name[..match_start];
    let title = clean_title(title_part);

    // Check for special (S00)
    let (is_special, special_type) = if season == 0 {
        (true, detect_special_type(name))
    } else {
        (false, None)
    };

    // Try to extract episode title (text between episode number and quality tokens)
    let episode_title = extract_episode_title(name, caps.get(0)?.end());

    let year = extract_year(&title).map(|(y, _)| y);

    Some((
        title.clone(),
        ParsedMedia::Tv(TvParsed {
            show_name: title,
            season: Some(season),
            episodes: vec![episode],
            episode_title,
            is_special,
            special_type,
            year,
            ..Default::default()
        }),
        0.95,
    ))
}

/// Try parsing multi-episode range: S01E05-E07 or S01E05-07.
fn try_parse_multi_ep_range(name: &str) -> Option<(String, ParsedMedia, f64)> {
    let caps = MULTI_EP_RANGE_RE.captures(name)?;

    let season: u32 = caps.name("season")?.as_str().parse().ok()?;
    let start: u32 = caps.name("start")?.as_str().parse().ok()?;
    let end: u32 = caps.name("end")?.as_str().parse().ok()?;

    if end < start {
        return None;
    }

    let episodes: Vec<u32> = (start..=end).collect();
    let is_pack = episodes.len() > 5;

    let match_start = caps.get(0)?.start();
    let title_part = &name[..match_start];
    let title = clean_title(title_part);
    let year = extract_year(&title).map(|(y, _)| y);

    Some((
        title.clone(),
        ParsedMedia::Tv(TvParsed {
            show_name: title,
            season: Some(season),
            episodes,
            is_pack,
            year,
            ..Default::default()
        }),
        0.95,
    ))
}

/// Try parsing multi-episode consecutive: S01E05E06E07.
fn try_parse_multi_ep_consec(name: &str) -> Option<(String, ParsedMedia, f64)> {
    let caps = MULTI_EP_CONSEC_RE.captures(name)?;

    let season: u32 = caps.name("season")?.as_str().parse().ok()?;
    let eps_str = caps.name("eps")?.as_str();

    // Extract all episode numbers
    let episodes: Vec<u32> = EP_EXTRACT_RE
        .captures_iter(eps_str)
        .filter_map(|c| c.get(1)?.as_str().parse().ok())
        .collect();

    if episodes.len() < 2 {
        return None; // Need at least 2 for multi-ep
    }

    let is_pack = episodes.len() > 5;

    let match_start = caps.get(0)?.start();
    let title_part = &name[..match_start];
    let title = clean_title(title_part);
    let year = extract_year(&title).map(|(y, _)| y);

    Some((
        title.clone(),
        ParsedMedia::Tv(TvParsed {
            show_name: title,
            season: Some(season),
            episodes,
            is_pack,
            year,
            ..Default::default()
        }),
        0.95,
    ))
}

/// Try parsing alternative multi-episode: 3x12-13 (range), 3x12-13-14 (explicit list).
/// Two numbers = range (expand all between), three or more = explicit episode list.
fn try_parse_alt_multi_ep(name: &str) -> Option<(String, ParsedMedia, f64)> {
    let caps = ALT_MULTI_EP_RE.captures(name)?;

    let season: u32 = caps.name("season")?.as_str().parse().ok()?;
    let eps_str = caps.name("eps")?.as_str();

    let nums: Vec<u32> = eps_str
        .split(['-', '–'])
        .filter_map(|s| s.parse().ok())
        .collect();

    if nums.len() < 2 {
        return None;
    }

    let episodes = if nums.len() == 2 {
        // Range: 12-14 → [12, 13, 14]
        let (start, end) = (nums[0], nums[1]);
        if end < start {
            return None;
        }
        (start..=end).collect()
    } else {
        // Explicit list: 12-13-14 → [12, 13, 14]
        nums
    };

    let is_pack = episodes.len() > 5;

    let match_start = caps.get(0)?.start();
    let title_part = &name[..match_start];
    let title = clean_title(title_part);
    let year = extract_year(&title).map(|(y, _)| y);

    Some((
        title.clone(),
        ParsedMedia::Tv(TvParsed {
            show_name: title,
            season: Some(season),
            episodes,
            is_pack,
            year,
            ..Default::default()
        }),
        0.95,
    ))
}

/// Try parsing alternative 1x05 format.
fn try_parse_alt_format(name: &str) -> Option<(String, ParsedMedia, f64)> {
    let caps = ALT_FORMAT_RE.captures(name)?;

    let season: u32 = caps.name("season")?.as_str().parse().ok()?;
    let episode: u32 = caps.name("episode")?.as_str().parse().ok()?;

    let match_start = caps.get(0)?.start();
    let title_part = &name[..match_start];
    let title = clean_title(title_part);
    let year = extract_year(&title).map(|(y, _)| y);

    // Check for special (0x05)
    let (is_special, special_type) = if season == 0 {
        (true, detect_special_type(name))
    } else {
        (false, None)
    };

    let episode_title = extract_episode_title(name, caps.get(0)?.end());

    Some((
        title.clone(),
        ParsedMedia::Tv(TvParsed {
            show_name: title,
            season: Some(season),
            episodes: vec![episode],
            episode_title,
            is_special,
            special_type,
            year,
            ..Default::default()
        }),
        0.9,
    ))
}

/// Try parsing daily show format: 2024.01.15.
fn try_parse_daily(name: &str) -> Option<(String, ParsedMedia, f64)> {
    let caps = DAILY_RE.captures(name)?;

    let year: u32 = caps.name("year")?.as_str().parse().ok()?;
    let month: u32 = caps.name("month")?.as_str().parse().ok()?;
    let day: u32 = caps.name("day")?.as_str().parse().ok()?;

    // Validate date
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }

    let air_date = format!("{year:04}-{month:02}-{day:02}");

    let match_start = caps.get(0)?.start();
    let title_part = &name[..match_start];
    let title = clean_title(title_part);

    Some((
        title.clone(),
        ParsedMedia::Tv(TvParsed {
            show_name: title,
            air_date: Some(air_date),
            is_daily: true,
            year: Some(year),
            ..Default::default()
        }),
        0.85,
    ))
}

/// Try parsing season pack: S01, Season 1.
fn try_parse_season_pack(name: &str) -> Option<(String, ParsedMedia, f64)> {
    let caps = SEASON_PACK_RE.captures(name)?;

    let season: u32 = caps
        .name("s1")
        .or_else(|| caps.name("s2"))?
        .as_str()
        .parse()
        .ok()?;

    let match_start = caps.get(0)?.start();
    let title_part = &name[..match_start];
    let title = clean_title(title_part);
    let year = extract_year(&title).map(|(y, _)| y);

    Some((
        title.clone(),
        ParsedMedia::Tv(TvParsed {
            show_name: title,
            season: Some(season),
            is_pack: true,
            year,
            ..Default::default()
        }),
        0.8,
    ))
}

/// Try parsing miniseries part format.
fn try_parse_miniseries(name: &str) -> Option<(String, ParsedMedia, f64)> {
    let caps = PART_RE.captures(name)?;

    let part_str = caps.name("num")?.as_str();
    let part: u32 = match part_str.to_lowercase().as_str() {
        "one" => 1,
        "two" => 2,
        "three" => 3,
        "four" => 4,
        "five" => 5,
        "six" => 6,
        "seven" => 7,
        "eight" => 8,
        "nine" => 9,
        "ten" => 10,
        n => n.parse().ok()?,
    };

    let match_start = caps.get(0)?.start();
    let title_part = &name[..match_start];
    let title = clean_title(title_part);
    let year = extract_year(&title).map(|(y, _)| y);

    Some((
        title.clone(),
        ParsedMedia::Tv(TvParsed {
            show_name: title,
            part: Some(part),
            year,
            ..Default::default()
        }),
        0.75,
    ))
}

/// Try parsing anime OVA/ONA format: [Group] Show - OVA.
fn try_parse_anime_ova(name: &str) -> Option<(String, ParsedMedia, f64)> {
    let caps = ANIME_OVA_RE.captures(name)?;

    let title = caps.name("title")?.as_str().trim().to_string();
    let ova_type = caps.name("type")?.as_str();
    let ova_num: Option<u32> = caps.name("num").and_then(|m| m.as_str().parse().ok());

    let special_type = if ova_type.eq_ignore_ascii_case("ova") {
        SpecialType::Ova
    } else {
        SpecialType::Ona
    };

    let title_clean = normalize_spaces(&title);
    let year = extract_year(&title_clean).map(|(y, _)| y);

    Some((
        title_clean.clone(),
        ParsedMedia::Tv(TvParsed {
            show_name: title_clean,
            absolute_episode: ova_num,
            is_special: true,
            special_type: Some(special_type),
            year,
            ..Default::default()
        }),
        0.9,
    ))
}

/// Try parsing anime bracket format: [Group] Show - 123.
fn try_parse_anime_bracket(name: &str) -> Option<(String, ParsedMedia, f64)> {
    let caps = ANIME_BRACKET_RE.captures(name)?;

    let title = caps.name("title")?.as_str().trim().to_string();
    let episode: u32 = caps.name("ep")?.as_str().parse().ok()?;

    // Check for OVA/ONA
    let (is_special, special_type) = if name.to_lowercase().contains("ova") {
        (true, Some(SpecialType::Ova))
    } else if name.to_lowercase().contains("ona") {
        (true, Some(SpecialType::Ona))
    } else {
        (false, None)
    };

    let title_clean = normalize_spaces(&title);
    let year = extract_year(&title_clean).map(|(y, _)| y);

    Some((
        title_clean.clone(),
        ParsedMedia::Tv(TvParsed {
            show_name: title_clean,
            absolute_episode: Some(episode),
            is_special,
            special_type,
            year,
            ..Default::default()
        }),
        0.9,
    ))
}

/// Try parsing anime absolute episode numbering.
fn try_parse_anime_absolute(name: &str) -> Option<(String, ParsedMedia, f64)> {
    // Only use this for names that look like anime (don't have SxxExx)
    if STANDARD_RE.is_match(name) || ALT_FORMAT_RE.is_match(name) {
        return None;
    }

    let caps = ANIME_ABSOLUTE_RE.captures(name)?;

    let episode: u32 = caps.name("ep")?.as_str().parse().ok()?;

    // Episode number must be reasonable (2-3 digits typically)
    if episode > 999 {
        return None;
    }

    let match_start = caps.get(0)?.start();
    let title_part = &name[..match_start];
    let title = clean_title(title_part);
    let year = extract_year(&title).map(|(y, _)| y);

    // Lower confidence since this is a fallback
    Some((
        title.clone(),
        ParsedMedia::Tv(TvParsed {
            show_name: title,
            absolute_episode: Some(episode),
            year,
            ..Default::default()
        }),
        0.6,
    ))
}

// =============================================================================
// Helpers
// =============================================================================

/// Quality tokens for episode title extraction.
static EP_TITLE_QUALITY_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(?:720|1080|2160|4k|web|blu|hdtv|proper|repack)").unwrap());

/// Extract episode title from the portion after the episode number.
fn extract_episode_title(name: &str, start_pos: usize) -> Option<String> {
    if start_pos >= name.len() {
        return None;
    }

    // Strip file extension before extracting episode title
    let name = super::common::remove_extension(name);
    let rest = name.get(start_pos..)?;

    // Find where quality tokens start
    let quality_re = &EP_TITLE_QUALITY_RE;

    let end_pos = quality_re
        .find(rest)
        .map(|m| m.start())
        .unwrap_or(rest.len());

    if end_pos < 2 {
        return None;
    }

    let title_part = &rest[..end_pos];
    let title = title_part.replace(['.', '_', '-'], " ").trim().to_string();

    if title.is_empty() || title.len() < 3 {
        None
    } else {
        Some(normalize_spaces(&title))
    }
}

/// Detect the type of special based on keywords in the name.
fn detect_special_type(name: &str) -> Option<SpecialType> {
    let lower = name.to_lowercase();

    // Holiday specials
    for (keyword, holiday) in [
        ("christmas", "christmas"),
        ("xmas", "christmas"),
        ("easter", "easter"),
        ("thanksgiving", "thanksgiving"),
        ("halloween", "halloween"),
        ("new.year", "new_year"),
        ("holiday", "holiday"),
    ] {
        if lower.contains(keyword) {
            return Some(SpecialType::Holiday {
                holiday: holiday.to_string(),
            });
        }
    }

    // Other special types
    if lower.contains("pilot") || lower.contains("unaired.pilot") {
        return Some(SpecialType::Pilot);
    }
    if lower.contains("behind.the.scenes") || lower.contains("bts") || lower.contains("making.of") {
        return Some(SpecialType::BehindTheScenes);
    }
    if lower.contains("recap") || lower.contains("summary") || lower.contains("previously") {
        return Some(SpecialType::Recap);
    }
    if lower.contains("ova") || lower.contains("original.video.animation") {
        return Some(SpecialType::Ova);
    }
    if lower.contains("ona") || lower.contains("original.net.animation") {
        return Some(SpecialType::Ona);
    }
    if lower.contains("crossover") {
        return Some(SpecialType::Crossover { other_show: None });
    }
    if lower.contains("movie") && !lower.contains("movie.name") {
        return Some(SpecialType::Movie);
    }

    // Generic special
    Some(SpecialType::Other)
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_standard_format() {
        let (title, media, conf) = parse_tv("Game.of.Thrones.S01E05.1080p.BluRay.x264-GROUP");
        assert_eq!(title, "Game of Thrones");
        assert!(conf > 0.9);

        if let ParsedMedia::Tv(tv) = media {
            assert_eq!(tv.season, Some(1));
            assert_eq!(tv.episodes, vec![5]);
            assert!(!tv.is_pack);
            assert!(!tv.is_special);
        } else {
            panic!("Expected TV");
        }
    }

    #[test]
    fn test_multi_ep_range() {
        let (_, media, _) = parse_tv("Show.S01E05-E07.1080p.WEB-DL");

        if let ParsedMedia::Tv(tv) = media {
            assert_eq!(tv.season, Some(1));
            assert_eq!(tv.episodes, vec![5, 6, 7]);
        } else {
            panic!("Expected TV");
        }
    }

    #[test]
    fn test_multi_ep_range_short() {
        let (_, media, _) = parse_tv("Show.S01E05-07.1080p.WEB-DL");

        if let ParsedMedia::Tv(tv) = media {
            assert_eq!(tv.season, Some(1));
            assert_eq!(tv.episodes, vec![5, 6, 7]);
        } else {
            panic!("Expected TV");
        }
    }

    #[test]
    fn test_multi_ep_consecutive() {
        let (_, media, _) = parse_tv("Show.S01E05E06E07.1080p.WEB-DL");

        if let ParsedMedia::Tv(tv) = media {
            assert_eq!(tv.season, Some(1));
            assert_eq!(tv.episodes, vec![5, 6, 7]);
        } else {
            panic!("Expected TV");
        }
    }

    #[test]
    fn test_alt_format() {
        let (title, media, _) = parse_tv("Show.Name.1x05.Title.720p.HDTV");
        assert_eq!(title, "Show Name");

        if let ParsedMedia::Tv(tv) = media {
            assert_eq!(tv.season, Some(1));
            assert_eq!(tv.episodes, vec![5]);
        } else {
            panic!("Expected TV");
        }
    }

    #[test]
    fn test_season_pack() {
        let (title, media, _) = parse_tv("Show.Name.S01.1080p.BluRay.x264");
        assert_eq!(title, "Show Name");

        if let ParsedMedia::Tv(tv) = media {
            assert_eq!(tv.season, Some(1));
            assert!(tv.is_pack);
            assert!(tv.episodes.is_empty());
        } else {
            panic!("Expected TV");
        }
    }

    #[test]
    fn test_daily_show() {
        let (title, media, _) = parse_tv("Daily.Show.2024.01.15.720p.HDTV");
        assert_eq!(title, "Daily Show");

        if let ParsedMedia::Tv(tv) = media {
            assert_eq!(tv.air_date, Some("2024-01-15".to_string()));
            assert!(tv.is_daily);
        } else {
            panic!("Expected TV");
        }
    }

    #[test]
    fn test_anime_bracket() {
        let (title, media, _) = parse_tv("[SubGroup] Anime Name - 123 [1080p].mkv");
        assert_eq!(title, "Anime Name");

        if let ParsedMedia::Tv(tv) = media {
            assert_eq!(tv.absolute_episode, Some(123));
        } else {
            panic!("Expected TV");
        }
    }

    #[test]
    fn test_miniseries_part() {
        let (title, media, _) = parse_tv("Band.of.Brothers.Part.3.1080p.BluRay");
        assert_eq!(title, "Band of Brothers");

        if let ParsedMedia::Tv(tv) = media {
            assert_eq!(tv.part, Some(3));
        } else {
            panic!("Expected TV");
        }
    }

    #[test]
    fn test_special_s00() {
        let (_, media, _) = parse_tv("Show.S00E05.Christmas.Special.1080p.WEB-DL");

        if let ParsedMedia::Tv(tv) = media {
            assert!(tv.is_special);
            assert_eq!(tv.season, Some(0));
            assert_eq!(tv.episodes, vec![5]);
            assert!(matches!(tv.special_type, Some(SpecialType::Holiday { .. })));
        } else {
            panic!("Expected TV");
        }
    }

    #[test]
    fn test_special_0x() {
        let (_, media, _) = parse_tv("Show.0x05.Title.720p");

        if let ParsedMedia::Tv(tv) = media {
            assert!(tv.is_special);
            assert_eq!(tv.season, Some(0));
        } else {
            panic!("Expected TV");
        }
    }

    #[test]
    fn test_anime_ova() {
        let (_, media, _) = parse_tv("[Group] Anime Name - OVA [1080p].mkv");

        if let ParsedMedia::Tv(tv) = media {
            assert!(tv.is_special);
            assert!(matches!(tv.special_type, Some(SpecialType::Ova)));
        } else {
            panic!("Expected TV");
        }
    }

    #[test]
    fn test_episode_title_extraction() {
        let (_, media, _) = parse_tv("Game.of.Thrones.S01E05.The.Wolf.and.the.Lion.1080p.BluRay");

        if let ParsedMedia::Tv(tv) = media {
            assert_eq!(tv.episode_title, Some("The Wolf and the Lion".to_string()));
        } else {
            panic!("Expected TV");
        }
    }

    #[test]
    fn test_episode_title_strips_extension() {
        let (_, media, _) = parse_tv("Barry - S04E01 - yikes.mkv");

        if let ParsedMedia::Tv(tv) = media {
            assert_eq!(tv.episode_title, Some("yikes".to_string()));
        } else {
            panic!("Expected TV");
        }
    }

    #[test]
    fn test_alt_format_extracts_episode_title() {
        // Real title is "Chapter One: Make Your Mark" but : is filesystem-escaped to -
        let (_, media, _) = parse_tv("Barry - 1x01 - Chapter One- Make Your Mark.mkv");

        if let ParsedMedia::Tv(tv) = media {
            assert_eq!(tv.season, Some(1));
            assert_eq!(tv.episodes, vec![1]);
            assert_eq!(
                tv.episode_title.as_deref(),
                Some("Chapter One Make Your Mark")
            );
        } else {
            panic!("Expected TV");
        }
    }

    #[test]
    fn test_alt_multi_ep_range() {
        let (title, media, conf) =
            parse_tv("It's Always Sunny in Philadelphia - 3x12-13 - The Gang Gets Whacked,.avi");
        assert_eq!(title, "It's Always Sunny in Philadelphia");
        assert!(conf > 0.9);

        if let ParsedMedia::Tv(tv) = media {
            assert_eq!(tv.season, Some(3));
            assert_eq!(tv.episodes, vec![12, 13]);
        } else {
            panic!("Expected TV");
        }
    }

    #[test]
    fn test_alt_multi_ep_range_dotted() {
        let (title, media, _) = parse_tv("Show.Name.1x05-07.Title.720p.HDTV");
        assert_eq!(title, "Show Name");

        if let ParsedMedia::Tv(tv) = media {
            assert_eq!(tv.season, Some(1));
            assert_eq!(tv.episodes, vec![5, 6, 7]);
        } else {
            panic!("Expected TV");
        }
    }

    #[test]
    fn test_alt_multi_ep_explicit_list() {
        let (title, media, _) = parse_tv("Show Name - 3x12-13-14 - Episode Title.avi");
        assert_eq!(title, "Show Name");

        if let ParsedMedia::Tv(tv) = media {
            assert_eq!(tv.season, Some(3));
            assert_eq!(tv.episodes, vec![12, 13, 14]);
        } else {
            panic!("Expected TV");
        }
    }

    #[test]
    fn test_show_with_year() {
        let (title, media, _) = parse_tv("Doctor.Who.2005.S01E01.1080p.BluRay");
        assert!(title.contains("Doctor Who"));

        if let ParsedMedia::Tv(tv) = media {
            assert_eq!(tv.year, Some(2005));
            assert_eq!(tv.season, Some(1));
        } else {
            panic!("Expected TV");
        }
    }

    /// Regression: S02E1-E16 must be treated as a season pack since it covers >5 episodes.
    /// Previously parsed as multi-ep range with is_pack=false, bypassing pack_preference rejection.
    #[test]
    fn test_multi_ep_range_over_five_is_pack() {
        let (title, media, _) =
            parse_tv("High.Potential.S02E1-E16.1080p.DSNP.WEB-DL.DDP5.1.H.264-GP-TV-NLsubs-Retail");
        assert_eq!(title, "High Potential");

        if let ParsedMedia::Tv(tv) = media {
            assert_eq!(tv.season, Some(2));
            assert_eq!(tv.episodes, (1..=16).collect::<Vec<u32>>());
            assert!(
                tv.is_pack,
                "S02E1-E16 (>5 episodes) should be treated as pack"
            );
        } else {
            panic!("Expected TV");
        }
    }

    #[test]
    fn test_multi_ep_range_five_or_fewer_not_pack() {
        // 3 episodes: not a pack
        let (_, media, _) = parse_tv("Show.S01E05-E07.1080p.WEB-DL");
        if let ParsedMedia::Tv(tv) = media {
            assert_eq!(tv.episodes, vec![5, 6, 7]);
            assert!(!tv.is_pack);
        } else {
            panic!("Expected TV");
        }

        // 5 episodes: still not a pack (threshold is >5)
        let (_, media, _) = parse_tv("Show.S01E01-E05.1080p.WEB-DL");
        if let ParsedMedia::Tv(tv) = media {
            assert_eq!(tv.episodes, vec![1, 2, 3, 4, 5]);
            assert!(!tv.is_pack);
        } else {
            panic!("Expected TV");
        }

        // 6 episodes: is a pack
        let (_, media, _) = parse_tv("Show.S01E01-E06.1080p.WEB-DL");
        if let ParsedMedia::Tv(tv) = media {
            assert_eq!(tv.episodes, vec![1, 2, 3, 4, 5, 6]);
            assert!(tv.is_pack, "6-episode range should be treated as pack");
        } else {
            panic!("Expected TV");
        }
    }

    #[test]
    fn test_alt_multi_ep_range_over_five_is_pack() {
        let (_, media, _) = parse_tv("Show.Name.1x01-10.Title.720p.HDTV");
        if let ParsedMedia::Tv(tv) = media {
            assert_eq!(tv.episodes, (1..=10).collect::<Vec<u32>>());
            assert!(
                tv.is_pack,
                "alt multi-ep 1x01-10 (>5 episodes) should be pack"
            );
        } else {
            panic!("Expected TV");
        }
    }
}
