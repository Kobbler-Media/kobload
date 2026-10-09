//! Game release name parsing.
//!
//! Handles:
//! - Standard: `Game.Name.v1.2.3-RELOADED`
//! - Platform: `Game.Name.PS5-GROUP`
//! - ISO vs RIP: `Game.Name.ISO-GROUP`
//! - DLC: `Game.Name.DLC.Pack-GROUP`
//! - Update: `Game.Name.Update.v1.5-GROUP`

use regex::Regex;
use std::sync::LazyLock;

use super::common::clean_title;
use super::types::{GameParsed, ParsedMedia};

// =============================================================================
// Regexes
// =============================================================================

/// Version pattern: v1.2.3, v1.2, Update v1.5
static VERSION_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(?:Update\.?)?v?(?P<ver>\d+(?:\.\d+)+)").unwrap());

/// Platform patterns
static PLATFORM_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?P<platform>PC|PS[345]|PlayStation[345]?|Xbox(?:One|360|SeriesX?)?|Switch|NSW|Nintendo|Wii|WiiU|3DS|PSP|PSVita|MAC|macOS|Linux)\b").unwrap()
});

/// Release type patterns
static RELEASE_TYPE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?P<type>ISO|RIP|GOG|Steam|EGS|Origin|Uplay|Razor1911)\b").unwrap()
});

/// DLC indicator
static DLC_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\b(?:DLC|Expansion|Add-?on|Season\.?Pass)\b").unwrap());

/// Update indicator
static UPDATE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\bUpdate\b").unwrap());

/// Crack indicators
static CRACK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)(?:\[c\]|Cracked|CRACK(?:ED)?|NoSteam|NoCD|NoDVD)").unwrap());

/// Release group pattern
static GROUP_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"-([A-Za-z0-9]+)(?:\.[a-z]{2,4})?$").unwrap());

// =============================================================================
// Parsing
// =============================================================================

/// Parse a game release name.
pub fn parse_game(name: &str) -> (String, ParsedMedia, f64) {
    let version = extract_version(name);
    let platform = extract_platform(name);
    let release_type = extract_release_type(name);
    let is_dlc = DLC_RE.is_match(name);
    let is_update = UPDATE_RE.is_match(name);
    let is_cracked = CRACK_RE.is_match(name);

    // Clean up title
    let title = extract_game_title(name);

    let confidence = if platform.is_some() || version.is_some() {
        0.85
    } else {
        0.6
    };

    (
        title.clone(),
        ParsedMedia::Game(GameParsed {
            title,
            platform,
            version,
            release_type,
            is_cracked,
            is_dlc,
            is_update,
        }),
        confidence,
    )
}

/// Extract version number.
fn extract_version(name: &str) -> Option<String> {
    VERSION_RE
        .captures(name)
        .and_then(|c| c.name("ver"))
        .map(|m| m.as_str().to_string())
}

/// Extract platform.
fn extract_platform(name: &str) -> Option<String> {
    PLATFORM_RE
        .captures(name)
        .and_then(|c| c.name("platform"))
        .map(|m| normalize_platform(m.as_str()))
}

/// Normalize platform name.
fn normalize_platform(platform: &str) -> String {
    match platform.to_lowercase().as_str() {
        "pc" => "PC".to_string(),
        "ps3" | "playstation3" => "PS3".to_string(),
        "ps4" | "playstation4" => "PS4".to_string(),
        "ps5" | "playstation5" => "PS5".to_string(),
        "xboxone" => "Xbox One".to_string(),
        "xbox360" => "Xbox 360".to_string(),
        "xboxseriesx" | "xboxseries" => "Xbox Series X".to_string(),
        "switch" | "nsw" | "nintendo" => "Switch".to_string(),
        "wiiu" => "Wii U".to_string(),
        "wii" => "Wii".to_string(),
        "3ds" => "3DS".to_string(),
        "psp" => "PSP".to_string(),
        "psvita" => "PS Vita".to_string(),
        "mac" | "macos" => "macOS".to_string(),
        "linux" => "Linux".to_string(),
        _ => platform.to_string(),
    }
}

/// Extract release type.
fn extract_release_type(name: &str) -> Option<String> {
    RELEASE_TYPE_RE
        .captures(name)
        .and_then(|c| c.name("type"))
        .map(|m| m.as_str().to_uppercase())
}

/// Extract game title (remove version, platform, group, etc.).
fn extract_game_title(name: &str) -> String {
    // Remove release group
    let cleaned = GROUP_RE.replace(name, "");
    // Remove version
    let cleaned = VERSION_RE.replace_all(&cleaned, " ");
    // Remove platform
    let cleaned = PLATFORM_RE.replace_all(&cleaned, " ");
    // Remove release type
    let cleaned = RELEASE_TYPE_RE.replace_all(&cleaned, " ");
    // Remove DLC/Update markers
    let cleaned = DLC_RE.replace_all(&cleaned, " ");
    let cleaned = UPDATE_RE.replace_all(&cleaned, " ");
    // Remove crack markers
    let cleaned = CRACK_RE.replace_all(&cleaned, " ");

    clean_title(&cleaned)
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_game_with_version() {
        let (title, media, _) = parse_game("Cyberpunk.2077.v1.6.3-RELOADED");
        assert!(title.contains("Cyberpunk"));

        if let ParsedMedia::Game(g) = media {
            assert_eq!(g.version.as_deref(), Some("1.6.3"));
        } else {
            panic!("Expected Game");
        }
    }

    #[test]
    fn test_game_platform() {
        let (_, media, _) = parse_game("Game.Name.PS5-GROUP");

        if let ParsedMedia::Game(g) = media {
            assert_eq!(g.platform.as_deref(), Some("PS5"));
        } else {
            panic!("Expected Game");
        }
    }

    #[test]
    fn test_game_dlc() {
        let (_, media, _) = parse_game("Game.Name.DLC.Pack-GROUP");

        if let ParsedMedia::Game(g) = media {
            assert!(g.is_dlc);
        } else {
            panic!("Expected Game");
        }
    }

    #[test]
    fn test_game_update() {
        let (_, media, _) = parse_game("Game.Name.Update.v1.5-GROUP");

        if let ParsedMedia::Game(g) = media {
            assert!(g.is_update);
            assert_eq!(g.version.as_deref(), Some("1.5"));
        } else {
            panic!("Expected Game");
        }
    }

    #[test]
    fn test_game_cracked() {
        let (_, media, _) = parse_game("Game.Name.[c]-GROUP");

        if let ParsedMedia::Game(g) = media {
            assert!(g.is_cracked);
        } else {
            panic!("Expected Game");
        }
    }

    #[test]
    fn test_game_iso() {
        let (_, media, _) = parse_game("Game.Name.ISO-GROUP");

        if let ParsedMedia::Game(g) = media {
            assert_eq!(g.release_type.as_deref(), Some("ISO"));
        } else {
            panic!("Expected Game");
        }
    }
}
