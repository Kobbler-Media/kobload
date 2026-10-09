//! Book release name parsing.
//!
//! Handles:
//! - Author-Title: `Brandon.Sanderson-Mistborn`
//! - With series: `Author - Series 01 - Title`
//! - Format hint: `Book.Title.[EPUB]`, `(retail)`
//! - Collection: `Author - Complete Works`

use regex::Regex;
use std::sync::LazyLock;

use super::common::{RELEASE_GROUP_RE, extract_year, normalize_spaces, remove_extension};
use super::types::{BookParsed, ParsedMedia};

// =============================================================================
// Regexes
// =============================================================================

/// Series pattern: Series Name 01 or (Series #1) or [Series Book 1]
static SERIES_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?ix)
        (?:
            \((?P<s1>[^)]+?)(?:\s*(?:\#|Book|Bk\.?|Vol\.?|Volume|No\.?)\s*)?(?P<n1>[\d.]+(?:-[\d.]+)?|[IVXLCDM]+)\)|
            \[(?P<s2>[^\]]+?)(?:\s*(?:\#|Book|Bk\.?|Vol\.?|Volume|No\.?)\s*)?(?P<n2>[\d.]+(?:-[\d.]+)?|[IVXLCDM]+)\]|
            (?P<s3>(?:Series|Saga|Chronicles?|Trilogy|Duology)\s+[\w\s]+?)(?:\s*(?:\#|Book|Bk\.?|Vol\.?|Volume|No\.?)\s*)?(?P<n3>[\d.]+(?:-[\d.]+)?|[IVXLCDM]+)|
            (?:Book|Bk\.?|Vol\.?|Volume|No\.?)\s*(?P<n4>[\d.]+(?:-[\d.]+)?|[IVXLCDM]+)
        )
    ").unwrap()
});

/// Format hint in brackets: [EPUB], (MOBI), [retail]
static FORMAT_HINT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)[\[\(](epub|mobi|azw3?|pdf|cbz|cbr|retail|scan|ocr)[\]\)]").unwrap()
});

/// Collection indicators
static COLLECTION_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:complete|collected|anthology|omnibus|compendium|boxed?\s*set|compilation)")
        .unwrap()
});

// =============================================================================
// Parsing
// =============================================================================

/// Parse a book release name.
pub fn parse_book(name: &str) -> (String, ParsedMedia, f64) {
    // Extract format hint first (so we can remove it from title)
    let format_hint = extract_format_hint(name);

    // Check for collection
    let is_collection = COLLECTION_RE.is_match(name);

    // Extract series info
    let (series, series_position) = extract_series(name);

    // Extract year
    let year = extract_year(name).map(|(y, _)| y);

    // Try to split author and title
    let (author, title) = extract_author_title(name);

    // Calculate confidence
    let confidence = if author.is_some() { 0.85 } else { 0.6 };

    let title_clean = title.clone();

    (
        title_clean.clone(),
        ParsedMedia::Book(BookParsed {
            title: title_clean,
            author,
            series,
            series_position,
            format_hint,
            is_collection,
            year,
        }),
        confidence,
    )
}

/// Extract format hint from brackets.
fn extract_format_hint(name: &str) -> Option<String> {
    FORMAT_HINT_RE
        .captures(name)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_lowercase())
}

/// Extract series name and position.
fn extract_series(name: &str) -> (Option<String>, Option<String>) {
    if let Some(caps) = SERIES_RE.captures(name) {
        // Try each capture group
        for (series_group, num_group) in [("s1", "n1"), ("s2", "n2"), ("s3", "n3")] {
            if let (Some(series), Some(num)) = (caps.name(series_group), caps.name(num_group)) {
                return (
                    Some(normalize_spaces(series.as_str())),
                    Some(num.as_str().to_string()),
                );
            }
        }
        // Just book number
        if let Some(num) = caps.name("n4") {
            return (None, Some(num.as_str().to_string()));
        }
    }
    (None, None)
}

/// Clean a book title - lighter than clean_title() since books don't have
/// quality tokens (720p, BluRay, x264, etc.) in their filenames.
fn clean_book_title(name: &str) -> String {
    let name = remove_extension(name);
    let name = name.replace(['.', '_'], " ");
    let name = name.replace(" - ", " ");
    let name = normalize_spaces(&name);
    name.trim_end_matches(['-', '–', '—', '.', '_'])
        .trim()
        .to_string()
}

/// Try to extract author and title.
fn extract_author_title(name: &str) -> (Option<String>, String) {
    // Remove format hints, series info, and release group
    let cleaned = FORMAT_HINT_RE.replace_all(name, " ");
    let cleaned = SERIES_RE.replace_all(&cleaned, " ");
    let cleaned = RELEASE_GROUP_RE.replace(&cleaned, "");
    let cleaned = normalize_spaces(&cleaned);

    // Try splitting on " - " first (most reliable)
    if let Some(pos) = cleaned.find(" - ") {
        let author = normalize_spaces(&cleaned[..pos]);
        let title = normalize_spaces(&cleaned[pos + 3..]);

        // Author names shouldn't be too long
        if author.split_whitespace().count() <= 6 && !title.is_empty() {
            return (Some(author), clean_book_title(&title));
        }
    }

    // Try splitting on dot-dash separator (scene format: Author.Name-Book.Title)
    // Replace dots with spaces first, then look for " - " or standalone "-" as separator
    let dotless = cleaned.replace('.', " ");
    let dotless = normalize_spaces(&dotless);
    // Find `-` that acts as author-title separator (not inside hyphenated words)
    if let Some(pos) = dotless.find('-') {
        let author = normalize_spaces(&dotless[..pos]);
        let title = normalize_spaces(&dotless[pos + 1..]);

        // Require at least 2 words in author to avoid false splits
        // on hyphenated titles like "Spider-Man" where "Spider" becomes author
        if author.split_whitespace().count() >= 2
            && author.split_whitespace().count() <= 6
            && !title.is_empty()
        {
            return (Some(author), clean_book_title(&title));
        }
    }

    // Fallback: no author detected
    (None, clean_book_title(&cleaned))
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_author_title_dash() {
        let (_title, media, _) = parse_book("Brandon Sanderson - Mistborn [EPUB]");

        if let ParsedMedia::Book(b) = media {
            assert_eq!(b.author.as_deref(), Some("Brandon Sanderson"));
            assert!(b.title.contains("Mistborn"));
            assert_eq!(b.format_hint.as_deref(), Some("epub"));
        } else {
            panic!("Expected Book");
        }
    }

    #[test]
    fn test_book_with_series() {
        let (_, media, _) = parse_book("Author Name - Series Name Book 3 - Title Here [EPUB]");

        if let ParsedMedia::Book(b) = media {
            assert!(b.series_position.is_some() || b.series.is_some());
        } else {
            panic!("Expected Book");
        }
    }

    #[test]
    fn test_format_hint() {
        let (_, media, _) = parse_book("Book.Title.[retail].epub");

        if let ParsedMedia::Book(b) = media {
            assert_eq!(b.format_hint.as_deref(), Some("retail"));
        } else {
            panic!("Expected Book");
        }
    }

    #[test]
    fn test_collection() {
        let (_, media, _) = parse_book("Author Name - Complete Works [EPUB]");

        if let ParsedMedia::Book(b) = media {
            assert!(b.is_collection);
        } else {
            panic!("Expected Book");
        }
    }

    #[test]
    fn test_series_in_parens() {
        let (_, media, _) = parse_book("Title (Series Name #3) [EPUB]");

        if let ParsedMedia::Book(b) = media {
            assert!(b.series.is_some() || b.series_position.is_some());
        } else {
            panic!("Expected Book");
        }
    }

    // --- Task 2: False author prevention ---

    #[test]
    fn test_title_only_no_false_author() {
        let (_, media, _) = parse_book("The.Hobbit.epub");
        if let ParsedMedia::Book(b) = media {
            assert!(b.author.is_none(), "Expected no author, got {:?}", b.author);
            assert!(b.title.contains("Hobbit"));
        } else {
            panic!("Expected Book");
        }
    }

    #[test]
    fn test_hyphenated_title_no_false_author() {
        let (_, media, _) = parse_book("Spider-Man.Into.The.Spider-Verse.epub");
        if let ParsedMedia::Book(b) = media {
            assert!(
                b.author.is_none(),
                "Expected no false author for hyphenated title, got {:?}",
                b.author
            );
        } else {
            panic!("Expected Book");
        }
    }

    #[test]
    fn test_dot_separated_author_title() {
        let (_, media, _) = parse_book("Brandon.Sanderson-Mistborn.The.Final.Empire.epub");
        if let ParsedMedia::Book(b) = media {
            assert_eq!(b.author.as_deref(), Some("Brandon Sanderson"));
            assert!(b.title.contains("Mistborn"));
            assert!(b.title.contains("Final Empire"));
        } else {
            panic!("Expected Book");
        }
    }

    // --- Task 2: Release group stripping ---

    #[test]
    fn test_release_group_stripped() {
        let (_, media, _) = parse_book("Brandon.Sanderson-Mistborn-FLiT");
        if let ParsedMedia::Book(b) = media {
            assert_eq!(b.author.as_deref(), Some("Brandon Sanderson"));
            assert!(
                !b.title.contains("FLiT"),
                "Release group should be stripped, got title {:?}",
                b.title
            );
        } else {
            panic!("Expected Book");
        }
    }

    // --- Task 3: Long author names ---

    #[test]
    fn test_long_author_name() {
        let (_, media, _) = parse_book("Sir Arthur Conan Doyle - The Hound of the Baskervilles");
        if let ParsedMedia::Book(b) = media {
            assert_eq!(b.author.as_deref(), Some("Sir Arthur Conan Doyle"));
            assert!(b.title.contains("Hound"));
        } else {
            panic!("Expected Book");
        }
    }

    // --- Task 4: Series regex improvements ---

    #[test]
    fn test_series_bk_abbreviation() {
        let (_, media, _) = parse_book("Title (Series Name Bk 3) [EPUB]");
        if let ParsedMedia::Book(b) = media {
            assert_eq!(b.series_position.as_deref(), Some("3"));
        } else {
            panic!("Expected Book");
        }
    }

    #[test]
    fn test_series_roman_numerals() {
        let (_, media, _) = parse_book("Title (Series Book IV) [EPUB]");
        if let ParsedMedia::Book(b) = media {
            assert_eq!(b.series_position.as_deref(), Some("IV"));
        } else {
            panic!("Expected Book");
        }
    }

    #[test]
    fn test_series_no_prefix() {
        let (_, media, _) = parse_book("Title (Series No. 5) [EPUB]");
        if let ParsedMedia::Book(b) = media {
            assert_eq!(b.series_position.as_deref(), Some("5"));
        } else {
            panic!("Expected Book");
        }
    }
}
