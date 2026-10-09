//! Music album release name parsing.
//!
//! Handles:
//! - Standard: `Artist-Album-2024-FLAC-GROUP`
//! - Vinyl: `Artist-Album-VINYL-2024-FLAC`
//! - Bootleg: `Artist-Live.At.Venue.2024-BOOTLEG`
//! - WEB: `Artist-Album-WEB-2024-FLAC`

use regex::Regex;
use std::sync::LazyLock;

use super::common::{clean_title, extract_year, normalize_spaces};
use super::types::{MusicParsed, ParsedMedia};

// =============================================================================
// Regexes
// =============================================================================

/// Scene music format: Artist-Album-Year-Format-GROUP
static SCENE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?P<artist>[^-]+)-(?P<album>[^-]+?)(?:-(?P<year>\d{4}))?(?:-(?P<format>FLAC|MP3|AAC|OPUS|OGG|WEB|CD|VINYL|TAPE))?(?:-[A-Z0-9]+)?$").unwrap()
});

/// Music source patterns
static SOURCE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(VINYL|CD|WEB|TAPE|CASSETTE|LP|DAT|HDCD|SACD|DVDA)\b").unwrap()
});

/// Release type patterns
static RELEASE_TYPE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\b(BOOTLEG|LIVE|COMPILATION|SINGLE|EP|PROMO|DEMO|REMASTER(?:ED)?|REISSUE|DELUXE)\b",
    )
    .unwrap()
});

/// Disc pattern
static DISC_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(?:CD|Disc|D)\.?\s*(?P<num>\d+)").unwrap());

// =============================================================================
// Parsing
// =============================================================================

/// Parse a music release name.
pub fn parse_music(name: &str) -> (String, ParsedMedia, f64) {
    // Try scene format first
    if let Some(result) = try_parse_scene_format(name) {
        return result;
    }

    // Fallback: extract components individually
    let source = extract_source(name);
    let release_type = extract_release_type(name);
    let year = extract_year(name).map(|(y, _)| y);
    let disc = extract_disc(name);

    // Try to split artist and album
    let (artist, album) = extract_artist_album(name);

    let confidence = if album.is_some() { 0.75 } else { 0.5 };

    (
        artist.clone(),
        ParsedMedia::Music(MusicParsed {
            artist,
            album,
            year,
            source,
            release_type,
            disc,
        }),
        confidence,
    )
}

/// Try parsing scene music format.
fn try_parse_scene_format(name: &str) -> Option<(String, ParsedMedia, f64)> {
    let caps = SCENE_RE.captures(name)?;

    let artist = caps.name("artist")?.as_str().replace(['.', '_'], " ");
    let album = caps.name("album")?.as_str().replace(['.', '_'], " ");
    let year: Option<u32> = caps.name("year").and_then(|m| m.as_str().parse().ok());

    let source = caps
        .name("format")
        .map(|m| m.as_str().to_lowercase())
        .or_else(|| extract_source(name));

    let release_type = extract_release_type(name);
    let disc = extract_disc(name);

    Some((
        normalize_spaces(&artist),
        ParsedMedia::Music(MusicParsed {
            artist: normalize_spaces(&artist),
            album: Some(normalize_spaces(&album)),
            year,
            source,
            release_type,
            disc,
        }),
        0.9,
    ))
}

/// Extract music source.
fn extract_source(name: &str) -> Option<String> {
    SOURCE_RE
        .captures(name)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_lowercase())
}

/// Extract release type.
/// Priority: bootleg > live > compilation > etc.
fn extract_release_type(name: &str) -> Option<String> {
    let lower = name.to_lowercase();

    // Check high-priority types first (more specific)
    if lower.contains("bootleg") {
        return Some("bootleg".to_string());
    }

    // Then check other types via regex
    RELEASE_TYPE_RE
        .captures(name)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_lowercase())
}

/// Extract disc number.
fn extract_disc(name: &str) -> Option<u32> {
    DISC_RE
        .captures(name)
        .and_then(|c| c.name("num"))
        .and_then(|m| m.as_str().parse().ok())
}

/// Extract artist and album from non-scene format.
fn extract_artist_album(name: &str) -> (String, Option<String>) {
    // Split on " - " BEFORE cleaning (clean_title replaces " - " with space)
    let raw = super::common::remove_extension(name);
    let raw = raw.replace(['.', '_'], " ");
    if let Some(pos) = raw.find(" - ") {
        let artist = normalize_spaces(&raw[..pos]);
        let album_raw = &raw[pos + 3..];
        // Clean quality tokens from album portion
        let album = normalize_spaces(&super::common::QUALITY_TOKENS.replace_all(album_raw, " "));
        if !album.is_empty() {
            return (artist, Some(album));
        }
    }

    (clean_title(name), None)
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scene_format() {
        let (artist, media, conf) = parse_music("Pink_Floyd-The_Wall-1979-FLAC-GROUP");
        assert!(conf > 0.8);

        if let ParsedMedia::Music(m) = media {
            assert!(artist.contains("Pink") || m.artist.contains("Pink"));
            assert!(m.album.as_deref().unwrap_or("").contains("Wall"));
            assert_eq!(m.year, Some(1979));
        } else {
            panic!("Expected Music");
        }
    }

    #[test]
    fn test_vinyl_source() {
        let (_, media, _) = parse_music("Artist-Album-VINYL-2024-FLAC");

        if let ParsedMedia::Music(m) = media {
            assert_eq!(m.source.as_deref(), Some("vinyl"));
        } else {
            panic!("Expected Music");
        }
    }

    #[test]
    fn test_bootleg() {
        let (_, media, _) = parse_music("Artist-Live.At.Venue-2024-BOOTLEG-MP3");

        if let ParsedMedia::Music(m) = media {
            assert_eq!(m.release_type.as_deref(), Some("bootleg"));
        } else {
            panic!("Expected Music");
        }
    }

    #[test]
    fn test_web_source() {
        let (_, media, _) = parse_music("Artist - Album (2024) [WEB FLAC]");

        if let ParsedMedia::Music(m) = media {
            assert_eq!(m.source.as_deref(), Some("web"));
        } else {
            panic!("Expected Music");
        }
    }

    #[test]
    fn test_non_scene_artist_album_split() {
        let (_, media, _) = parse_music("Guns N Roses - Greatest Hits (2024) [WEB FLAC 24bit]");

        if let ParsedMedia::Music(m) = media {
            assert_eq!(m.artist, "Guns N Roses");
            let album = m.album.as_deref().unwrap();
            assert!(
                album.contains("Greatest Hits"),
                "Album should contain 'Greatest Hits', got: {album}"
            );
        } else {
            panic!("Expected Music");
        }
    }

    #[test]
    fn test_artist_album_no_separator() {
        let (_, media, _) = parse_music("SomeAlbumOnly");

        if let ParsedMedia::Music(m) = media {
            assert!(m.album.is_none());
        } else {
            panic!("Expected Music");
        }
    }
}
