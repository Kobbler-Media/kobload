//! Movie release name parsing.
//!
//! Handles:
//! - Standard: `Movie.Name.2024.1080p.BluRay`
//! - Edition: `Movie.Name.2024.Directors.Cut.1080p`
//! - Multi-part: `Movie.Name.2024.Part.1.1080p`
//! - 3D: `Movie.Name.2024.3D.1080p.BluRay`
//! - Multi-disc: `Movie.Name.2024.CD1.avi`

use regex::Regex;
use std::sync::LazyLock;

use super::common::{clean_pre_year_title, clean_title};
use super::types::{MovieParsed, ParsedMedia};

// =============================================================================
// Regexes
// =============================================================================

/// Edition patterns
static EDITION_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?ix)
        (?P<edition>
            directors?\.?cut|
            extended\.?(?:cut|edition)?|
            theatrical\.?(?:cut|edition)?|
            unrated\.?(?:cut|edition)?|
            uncut|
            imax\.?(?:edition)?|
            remastered|
            4k\.?remaster|
            criterion\.?(?:collection)?|
            special\.?edition|
            anniversary\.?(?:edition)?|
            \d+(?:th|st|nd|rd)\.?anniversary|
            collectors?\.?edition|
            ultimate\.?(?:edition|cut)|
            final\.?cut|
            redux|
            workprint|
            fanedit|
            fan\.?edit|
            regraded|
            hybrid|
            open\.?matte
        )",
    )
    .unwrap()
});

/// Part/disc patterns
static PART_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:Part|Pt)\.?\s*(?P<num>\d+|One|Two|Three|Four|Five)").unwrap()
});

/// Disc/CD patterns
static DISC_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(?:CD|Disc|DVD|BD)\.?\s*(?P<num>\d+)").unwrap());

/// 3D pattern
static THREE_D_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:^|[\s.\-\[\(])(?:3D|HSBS|SBS|HTAB|HOU|MVC)(?:[\s.\-\]\)]|$)").unwrap()
});

/// Quality tokens regex for find_release_year — marks where quality info starts.
static QUALITY_START_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:720|1080|2160|4k|uhd|bluray|blu-ray|web|hdtv|dvd|remux|hdr|dv)").unwrap()
});

/// Year pattern with word boundaries for movie release year detection.
/// Only captures the year digits; boundary checking is done in find_release_year
/// to avoid the regex consuming delimiters that adjacent years need.
static MOVIE_YEAR_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?:19|20)\d{2}").unwrap());

// =============================================================================
// Parsing
// =============================================================================

/// Parse a movie release name.
pub fn parse_movie(name: &str) -> (String, ParsedMedia, f64) {
    // Find all year candidates and pick the last one before quality tokens
    // This handles cases like "2001.A.Space.Odyssey.1968.1080p" where 2001 is part of title
    let (year, year_pos) = find_release_year(name);

    // Extract title (everything before year, or before quality tokens)
    let title = extract_movie_title(name, year_pos);

    // Extract edition
    let edition = extract_edition(name);

    // Extract part number
    let part = extract_part(name);

    // Extract disc number
    let disc = extract_disc(name);

    // Check for 3D
    let is_3d = THREE_D_RE.is_match(name);

    // Calculate confidence
    let confidence = if year.is_some() { 0.9 } else { 0.7 };

    (
        title.clone(),
        ParsedMedia::Movie(MovieParsed {
            title,
            year,
            edition,
            part,
            disc,
            is_3d,
        }),
        confidence,
    )
}

/// Extract the movie title from the release name.
fn extract_movie_title(name: &str, year_pos: Option<usize>) -> String {
    // If we have a year position, title is everything before it
    if let Some(pos) = year_pos {
        // The year regex captures digits but the preceding delimiter (e.g. '(' in "(2018)")
        // is NOT part of the capture. Trim any trailing delimiter so we don't get "Bao (".
        let title_part = name[..pos].trim_end_matches(|c: char| "([.- ".contains(c));
        return clean_pre_year_title(title_part);
    }

    // Otherwise, just clean the whole thing
    clean_title(name)
}

/// Find the release year in a movie name.
/// Picks the LAST year that appears before quality tokens.
/// This handles cases like "2001.A.Space.Odyssey.1968.1080p" correctly.
/// If the only year candidate is at position 0, it's likely part of the title
/// (e.g. "2001.A.Space.Odyssey.1080p") — return None.
fn find_release_year(name: &str) -> (Option<u32>, Option<usize>) {
    // Find where quality info starts (if any)
    let quality_pos = QUALITY_START_RE
        .find(name)
        .map(|m| m.start())
        .unwrap_or(name.len());

    // Only look for years before quality tokens
    let search_part = &name[..quality_pos];

    let mut years: Vec<(u32, usize)> = Vec::new();
    let delimiters = &[' ', '.', '-', '(', '[', ')', ']'];

    for m in MOVIE_YEAR_RE.find_iter(search_part) {
        // Check boundary before: must be start of string or a delimiter
        if m.start() > 0 {
            let prev = search_part.as_bytes()[m.start() - 1] as char;
            if !delimiters.contains(&prev) {
                continue;
            }
        }
        // Check boundary after: must be end of string or a delimiter
        if m.end() < search_part.len() {
            let next = search_part.as_bytes()[m.end()] as char;
            if !delimiters.contains(&next) {
                continue;
            }
        }
        if let Ok(y) = m.as_str().parse::<u32>() {
            years.push((y, m.start()));
        }
    }

    if years.is_empty() {
        return (None, None);
    }

    // If there's only one year candidate and it starts at position 0,
    // it's likely part of the title (e.g. "2001.A.Space.Odyssey.1080p")
    if years.len() == 1 && years[0].1 == 0 {
        return (None, None);
    }

    // Take the last year
    let (year, pos) = years[years.len() - 1];
    (Some(year), Some(pos))
}

/// Extract edition from the release name.
fn extract_edition(name: &str) -> Option<String> {
    let caps = EDITION_RE.captures(name)?;
    let edition = caps.name("edition")?.as_str();

    // Normalize the edition name
    let normalized = edition
        .replace(['.', '-'], " ")
        .to_lowercase()
        .split_whitespace()
        .map(|w| {
            let mut chars = w.chars();
            match chars.next() {
                Some(c) => c.to_uppercase().chain(chars).collect(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ");

    Some(normalized)
}

/// Extract part number from the release name.
fn extract_part(name: &str) -> Option<u32> {
    let caps = PART_RE.captures(name)?;
    let part_str = caps.name("num")?.as_str();

    match part_str.to_lowercase().as_str() {
        "one" => Some(1),
        "two" => Some(2),
        "three" => Some(3),
        "four" => Some(4),
        "five" => Some(5),
        n => n.parse().ok(),
    }
}

/// Extract disc number from the release name.
fn extract_disc(name: &str) -> Option<u32> {
    let caps = DISC_RE.captures(name)?;
    caps.name("num")?.as_str().parse().ok()
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_movie() {
        let (title, media, conf) = parse_movie("Inception.2010.1080p.BluRay.x264-GROUP");
        assert_eq!(title, "Inception");
        assert!(conf > 0.8);

        if let ParsedMedia::Movie(m) = media {
            assert_eq!(m.year, Some(2010));
            assert!(m.edition.is_none());
            assert!(!m.is_3d);
        } else {
            panic!("Expected Movie");
        }
    }

    #[test]
    fn test_movie_title_keeps_flag_word_mid_title() {
        // Regression: "Real" was stripped as the REAL release flag, giving
        // "The Odyssey The Story".
        let (title, _, _) =
            parse_movie("The.Odyssey.The.Real.Story.2026.1080p.FNDG.WEB-DL.AAC2.0.H.264-NYXIS");
        assert_eq!(title, "The Odyssey The Real Story");

        let (title, _, _) = parse_movie("Real.Steel.2011.1080p.BluRay.x264-GROUP");
        assert_eq!(title, "Real Steel");
    }

    #[test]
    fn test_movie_title_strips_trailing_flag_before_year() {
        let (title, _, _) =
            parse_movie("Dao.AKA.The.Blade.REPACK.1995.1080p.NF.WEB-DL.AAC2.0.x264-Gohan");
        assert_eq!(title, "Dao AKA The Blade");
    }

    #[test]
    fn test_movie_title_keeps_hyphenated_last_word() {
        let (title, _, _) = parse_movie("Spider-Man.2002.1080p.BluRay.x264-GROUP");
        assert_eq!(title, "Spider-Man");
    }

    #[test]
    fn test_movie_with_edition() {
        let (title, media, _) = parse_movie("Blade.Runner.1982.Directors.Cut.1080p.BluRay");
        assert_eq!(title, "Blade Runner");

        if let ParsedMedia::Movie(m) = media {
            assert_eq!(m.year, Some(1982));
            assert_eq!(m.edition.as_deref(), Some("Directors Cut"));
        } else {
            panic!("Expected Movie");
        }
    }

    #[test]
    fn test_movie_extended_edition() {
        let (_, media, _) = parse_movie("Lord.of.the.Rings.2001.Extended.Edition.1080p.BluRay");

        if let ParsedMedia::Movie(m) = media {
            assert_eq!(m.edition.as_deref(), Some("Extended Edition"));
        } else {
            panic!("Expected Movie");
        }
    }

    #[test]
    fn test_movie_unrated() {
        let (_, media, _) = parse_movie("Movie.Name.2020.Unrated.1080p.WEB-DL");

        if let ParsedMedia::Movie(m) = media {
            assert_eq!(m.edition.as_deref(), Some("Unrated"));
        } else {
            panic!("Expected Movie");
        }
    }

    #[test]
    fn test_movie_3d() {
        let (title, media, _) = parse_movie("Avatar.2009.3D.1080p.BluRay.HSBS");
        assert_eq!(title, "Avatar");

        if let ParsedMedia::Movie(m) = media {
            assert!(m.is_3d);
        } else {
            panic!("Expected Movie");
        }
    }

    #[test]
    fn test_movie_multipart() {
        let (title, media, _) = parse_movie("Kill.Bill.2003.Part.1.1080p.BluRay");
        assert_eq!(title, "Kill Bill");

        if let ParsedMedia::Movie(m) = media {
            assert_eq!(m.part, Some(1));
        } else {
            panic!("Expected Movie");
        }
    }

    #[test]
    fn test_movie_multidisc() {
        let (_, media, _) = parse_movie("Movie.Name.2000.CD1.avi");

        if let ParsedMedia::Movie(m) = media {
            assert_eq!(m.disc, Some(1));
        } else {
            panic!("Expected Movie");
        }
    }

    #[test]
    fn test_movie_anniversary() {
        let (_, media, _) =
            parse_movie("Jurassic.Park.1993.30th.Anniversary.4K.Remaster.2160p.BluRay");

        if let ParsedMedia::Movie(m) = media {
            // Should match "30th.anniversary" - verify it captures some edition
            assert!(m.edition.is_some());
        } else {
            panic!("Expected Movie");
        }
    }

    #[test]
    fn test_movie_criterion() {
        let (_, media, _) = parse_movie("Seven.Samurai.1954.Criterion.Collection.1080p.BluRay");

        if let ParsedMedia::Movie(m) = media {
            assert!(
                m.edition
                    .as_deref()
                    .unwrap()
                    .to_lowercase()
                    .contains("criterion")
            );
        } else {
            panic!("Expected Movie");
        }
    }

    #[test]
    fn test_movie_imax() {
        let (_, media, _) = parse_movie("Oppenheimer.2023.IMAX.2160p.WEB-DL");

        if let ParsedMedia::Movie(m) = media {
            assert_eq!(m.edition.as_deref(), Some("Imax"));
        } else {
            panic!("Expected Movie");
        }
    }

    #[test]
    fn test_movie_year_in_title() {
        // Films like "2001: A Space Odyssey" where the year is part of the title
        let (title, media, _) = parse_movie("2001.A.Space.Odyssey.1968.1080p.BluRay");

        if let ParsedMedia::Movie(m) = media {
            // Should extract 1968 as the release year
            assert_eq!(m.year, Some(1968));
            // Title cleaning is best-effort
            assert!(title.contains("Space Odyssey"));
        } else {
            panic!("Expected Movie");
        }
    }

    #[test]
    fn test_movie_year_as_title_no_release_year() {
        // When the only year is at position 0, it's part of the title
        let (title, media, _) = parse_movie("2001.A.Space.Odyssey.1080p.BluRay");

        if let ParsedMedia::Movie(m) = media {
            // 2001 is the title start, not a release year
            assert_eq!(m.year, None);
            assert!(
                title.contains("2001"),
                "Title should contain 2001, got: {title}"
            );
            assert!(
                title.contains("Space Odyssey"),
                "Title should contain Space Odyssey, got: {title}"
            );
        } else {
            panic!("Expected Movie");
        }
    }

    #[test]
    fn test_movie_year_in_parens_no_trailing_paren_in_title() {
        // "Bao (2018)" should produce title "Bao", not "Bao ("
        let (title, media, _) = parse_movie("Bao (2018).mkv");
        assert_eq!(title, "Bao");

        if let ParsedMedia::Movie(m) = media {
            assert_eq!(m.year, Some(2018));
        } else {
            panic!("Expected Movie");
        }
    }

    #[test]
    fn test_movie_year_in_title_with_release_year() {
        // "Blade Runner 2049 2017 PROPER 720p" — 2049 is part of the title, 2017 is the year
        let (title, media, _) =
            parse_movie("Blade Runner 2049 2017 PROPER 720p BluRay x264-BLOW.mkv");
        assert_eq!(title, "Blade Runner 2049");

        if let ParsedMedia::Movie(m) = media {
            assert_eq!(m.year, Some(2017));
        } else {
            panic!("Expected Movie");
        }
    }
}
