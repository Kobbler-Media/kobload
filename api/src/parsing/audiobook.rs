//! Audiobook release name parsing.
//!
//! Handles:
//! - Standard: `Author - Title (Read by Narrator)`
//! - With series: `Author - Series 01 - Title`
//! - Unabridged/Abridged indicators
//! - Multi-part: `Title Part 1 of 3`

use regex::Regex;
use std::sync::LazyLock;

use super::common::{clean_audiobook_title, extract_year, normalize_spaces, remove_extension};
use super::types::{AudiobookParsed, ParsedMedia};

// =============================================================================
// Regexes
// =============================================================================

/// Empty or whitespace-only parentheses/brackets left after stripping content.
static EMPTY_PARENS_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\(\s*\)|\[\s*\]").unwrap());

/// Narrator pattern: (Read by Name), Narrated by Name, [Narrator: Name]
static NARRATOR_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?ix)
        (?:
            \((?:Read|Narrated)\s+by\s+(?P<n1>[^)]+)\)|
            (?:Read|Narrated)\s+by\s+(?P<n2>[\w\s.]+?)(?:[\[\(\-]|$)|
            \[Narrator:\s*(?P<n3>[^\]]+)\]
        )
    ",
    )
    .unwrap()
});

/// Unabridged/abridged pattern
static UNABRIDGED_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)\bunabridged\b").unwrap());

static ABRIDGED_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)\babridged\b").unwrap());

/// Part pattern: Part 1, Part 1 of 3, Pt. 1/3
static PART_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:Part|Pt\.?)\s*(?P<part>\d+)(?:\s*(?:of|/)\s*(?P<total>\d+))?").unwrap()
});

/// "Title by Author" pattern — matches " by " followed by a person name (2-5 capitalized words)
static BY_AUTHOR_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\s+by\s+(?P<author>[A-Z][a-z]+(?:\s+[A-Z]\.?\s*)*(?:\s+[A-Za-z][a-z]+){1,4})\s*$",
    )
    .unwrap()
});

/// Series pattern (similar to books)
static SERIES_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?ix)
        (?:
            \((?P<s1>[^)]+?)(?:\s*(?:\#|Book|Vol\.?)\s*)?(?P<n1>\d+(?:-\d+)?)\)|
            \[(?P<s2>[^\]]+?)(?:\s*(?:\#|Book|Vol\.?)\s*)?(?P<n2>\d+(?:-\d+)?)\]|
            (?:Book|Vol\.?)\s*(?P<n3>\d+(?:-\d+)?)
        )
    ",
    )
    .unwrap()
});

// =============================================================================
// Parsing
// =============================================================================

/// Parse an audiobook release name.
pub fn parse_audiobook(name: &str) -> (String, ParsedMedia, f64) {
    // Extract narrator
    let narrator = extract_narrator(name);

    // Check unabridged status
    let is_unabridged = UNABRIDGED_RE.is_match(name) || !ABRIDGED_RE.is_match(name);

    // Extract part info
    let (part, total_parts) = extract_part_info(name);

    // Extract series
    let (series, series_position) = extract_series(name);

    // Extract year
    let year = extract_year(name).map(|(y, _)| y);

    // Try to split author and title
    let (author, title) = extract_author_title(name);

    // Calculate confidence
    let confidence = match (author.is_some(), narrator.is_some()) {
        (true, true) => 0.95,
        (true, false) => 0.8,
        (false, true) => 0.75,
        (false, false) => 0.5,
    };

    let title_clean = title.clone();

    (
        title_clean.clone(),
        ParsedMedia::Audiobook(AudiobookParsed {
            title: title_clean,
            author,
            narrator,
            series,
            series_position,
            is_unabridged,
            part,
            total_parts,
            year,
            is_generic_filename: false,
        }),
        confidence,
    )
}

/// Extract narrator name.
fn extract_narrator(name: &str) -> Option<String> {
    let caps = NARRATOR_RE.captures(name)?;

    for group in ["n1", "n2", "n3"] {
        if let Some(m) = caps.name(group) {
            let narrator = normalize_spaces(m.as_str());
            if !narrator.is_empty() {
                return Some(narrator);
            }
        }
    }
    None
}

/// Extract part number and total.
fn extract_part_info(name: &str) -> (Option<u32>, Option<u32>) {
    if let Some(caps) = PART_RE.captures(name) {
        let part: Option<u32> = caps.name("part").and_then(|m| m.as_str().parse().ok());
        let total: Option<u32> = caps.name("total").and_then(|m| m.as_str().parse().ok());
        return (part, total);
    }
    (None, None)
}

/// Extract series name and position.
fn extract_series(name: &str) -> (Option<String>, Option<String>) {
    if let Some(caps) = SERIES_RE.captures(name) {
        for (series_group, num_group) in [("s1", "n1"), ("s2", "n2")] {
            if let (Some(series), Some(num)) = (caps.name(series_group), caps.name(num_group)) {
                let series_text = normalize_spaces(series.as_str());
                // Reject if "series name" is purely digits — it's a track number
                // in parens like "(10)", not actual series info
                if series_text.chars().all(|c| c.is_ascii_digit()) {
                    continue;
                }
                return (Some(series_text), Some(num.as_str().to_string()));
            }
        }
        if let Some(num) = caps.name("n3") {
            return (None, Some(num.as_str().to_string()));
        }
    }
    (None, None)
}

/// Extract author and title.
fn extract_author_title(name: &str) -> (Option<String>, String) {
    // Remove extension and narrator info for cleaner parsing
    let no_ext = remove_extension(name);
    let cleaned = NARRATOR_RE.replace_all(&no_ext, " ");
    let cleaned = PART_RE.replace_all(&cleaned, " ");
    let cleaned = UNABRIDGED_RE.replace_all(&cleaned, " ");
    let cleaned = ABRIDGED_RE.replace_all(&cleaned, " ");
    let cleaned = EMPTY_PARENS_RE.replace_all(&cleaned, " ");
    let cleaned = normalize_spaces(&cleaned);

    // Try splitting on " - " (Author - Title)
    if let Some(pos) = cleaned.find(" - ") {
        let author = normalize_spaces(&cleaned[..pos]);
        let title = normalize_spaces(&cleaned[pos + 3..]);

        // Reject if "author" is purely digits (it's a track number, not a name)
        let is_digit_only = author.chars().all(|c| c.is_ascii_digit());
        if !is_digit_only && author.split_whitespace().count() <= 5 && !title.is_empty() {
            return (Some(author), clean_audiobook_title(&title));
        }
    }

    // Try "Title by Author" pattern (e.g., "A Kingdom of Flesh and Fire by Jennifer L. Armentrout")
    if let Some(author) = extract_by_author(&cleaned) {
        let by_pos = BY_AUTHOR_RE.find(&cleaned).unwrap().start();
        let title = normalize_spaces(&cleaned[..by_pos]);
        if !title.is_empty() {
            return (Some(author), clean_audiobook_title(&title));
        }
    }

    (None, clean_audiobook_title(&cleaned))
}

/// Extract author from "Title by Author.Name" pattern.
fn extract_by_author(name: &str) -> Option<String> {
    let caps = BY_AUTHOR_RE.captures(name)?;
    let author = normalize_spaces(caps.name("author")?.as_str());
    // Reject if it looks like a common word rather than a name
    let word_count = author.split_whitespace().count();
    if !(2..=5).contains(&word_count) {
        return None;
    }
    Some(author)
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_audiobook_with_narrator() {
        let (_, media, conf) =
            parse_audiobook("Brandon Sanderson - Mistborn (Read by Michael Kramer)");
        assert!(conf > 0.9);

        if let ParsedMedia::Audiobook(ab) = media {
            assert_eq!(ab.author.as_deref(), Some("Brandon Sanderson"));
            assert!(ab.title.contains("Mistborn"));
            assert_eq!(ab.narrator.as_deref(), Some("Michael Kramer"));
        } else {
            panic!("Expected Audiobook");
        }
    }

    #[test]
    fn test_unabridged() {
        let (_, media, _) = parse_audiobook("Title - Unabridged");

        if let ParsedMedia::Audiobook(ab) = media {
            assert!(ab.is_unabridged);
        } else {
            panic!("Expected Audiobook");
        }
    }

    #[test]
    fn test_multipart() {
        let (_, media, _) = parse_audiobook("Author - Title Part 2 of 5");

        if let ParsedMedia::Audiobook(ab) = media {
            assert_eq!(ab.part, Some(2));
            assert_eq!(ab.total_parts, Some(5));
        } else {
            panic!("Expected Audiobook");
        }
    }

    #[test]
    fn test_narrator_bracket() {
        let (_, media, _) = parse_audiobook("Title [Narrator: John Smith]");

        if let ParsedMedia::Audiobook(ab) = media {
            assert_eq!(ab.narrator.as_deref(), Some("John Smith"));
        } else {
            panic!("Expected Audiobook");
        }
    }

    #[test]
    fn test_title_by_author() {
        let (_, media, conf) =
            parse_audiobook("A Kingdom of Flesh and Fire by Jennifer L. Armentrout.m4b");
        // Confidence is 0.8 (author found, no narrator)
        assert!(conf >= 0.5, "conf was {conf}");

        if let ParsedMedia::Audiobook(ab) = media {
            assert_eq!(ab.author.as_deref(), Some("Jennifer L. Armentrout"));
            assert!(
                ab.title.contains("Kingdom of Flesh and Fire"),
                "title was: {}",
                ab.title
            );
        } else {
            panic!("Expected Audiobook");
        }
    }

    #[test]
    fn test_title_by_author_no_false_positive() {
        // "by" as part of a normal title (not followed by a person name)
        let (_, media, _) = parse_audiobook("Stand by Me.m4b");

        if let ParsedMedia::Audiobook(ab) = media {
            // Should NOT extract "Me" as an author
            assert!(
                ab.author.is_none(),
                "author should be None, got: {:?}",
                ab.author
            );
        } else {
            panic!("Expected Audiobook");
        }
    }
}
