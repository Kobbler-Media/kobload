//! XXX/Adult content release name parsing.
//!
//! Handles standard adult content naming patterns including
//! studio names and performer detection.

use regex::Regex;
use std::sync::LazyLock;

use super::common::{clean_title, extract_year, normalize_spaces};
use super::types::{ParsedMedia, XxxParsed};

// =============================================================================
// Regexes
// =============================================================================

/// Common studio patterns at the start of names
static STUDIO_RE: LazyLock<Regex> = LazyLock::new(|| {
    // Studios often appear at the start, followed by a separator
    Regex::new(r"^(?P<studio>[A-Za-z0-9]+(?:\s*[A-Za-z0-9]+)?)[\s.\-]+").unwrap()
});

/// Release group pattern
static GROUP_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"-([A-Za-z0-9]+)(?:\.[a-z]{2,4})?$").unwrap());

// =============================================================================
// Parsing
// =============================================================================

/// Parse an XXX release name.
pub fn parse_xxx(name: &str) -> (String, ParsedMedia, f64) {
    let year = extract_year(name).map(|(y, _)| y);

    // Try to extract studio (usually first part)
    let studio = extract_studio(name);

    // Clean up title
    let title = extract_title(name, studio.as_deref());

    // Performers are difficult to extract reliably without a database
    // Just return empty for now - caller can use external sources
    let performers = Vec::new();

    let confidence = if studio.is_some() { 0.7 } else { 0.5 };

    (
        title.clone(),
        ParsedMedia::Xxx(XxxParsed {
            title,
            studio,
            performers,
            year,
        }),
        confidence,
    )
}

/// Extract studio name from the start of the release name.
fn extract_studio(name: &str) -> Option<String> {
    // Studios are typically at the start, before the title
    // Common pattern: Studio - Title or Studio.Title
    let cleaned = name.replace(['.', '_'], " ");

    if let Some(pos) = cleaned.find(" - ") {
        let potential_studio = cleaned[..pos].trim();
        // Studios are usually 1-3 words
        if potential_studio.split_whitespace().count() <= 3 {
            return Some(normalize_spaces(potential_studio));
        }
    }

    // Try STUDIO_RE for simple cases
    STUDIO_RE
        .captures(&cleaned)
        .and_then(|c| c.name("studio"))
        .map(|m| normalize_spaces(m.as_str()))
}

/// Extract title (remove studio, release group, quality).
fn extract_title(name: &str, studio: Option<&str>) -> String {
    let cleaned = GROUP_RE.replace(name, "");
    let cleaned = cleaned.replace(['.', '_'], " ");

    // Remove studio if we extracted one
    let cleaned = if let Some(s) = studio {
        cleaned.replacen(s, "", 1)
    } else {
        cleaned.to_string()
    };

    // Remove leading separators
    let cleaned = cleaned.trim_start_matches([' ', '-', '.']);

    clean_title(cleaned)
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_xxx_basic() {
        let (title, media, _) = parse_xxx("Studio.Name.Title.Here.1080p-GROUP");

        if let ParsedMedia::Xxx(x) = media {
            assert!(!title.is_empty());
            // Basic parsing works
            assert!(x.performers.is_empty()); // Can't reliably extract without database
        } else {
            panic!("Expected Xxx");
        }
    }

    #[test]
    fn test_xxx_with_year() {
        let (_, media, _) = parse_xxx("Studio - Title 2024 1080p");

        if let ParsedMedia::Xxx(x) = media {
            assert_eq!(x.year, Some(2024));
        } else {
            panic!("Expected Xxx");
        }
    }
}
