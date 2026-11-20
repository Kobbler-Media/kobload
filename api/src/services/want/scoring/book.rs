//! Whether an ebook or audiobook release is the wanted book. Both formats share
//! these rules; only the parser differs.
//!
//! A release proves its author through the parsed `Author - Title` split, the
//! author's words around the title, or the indexer's own author field. With
//! that proof, extra title words are tolerated (subtitles, "A Novel"). Without
//! it the title must match exactly and the year must sit near publication:
//! "Michael.Katz.Krefeld.Mumien.2026" is not "Michael" by Bob Mortimer.
//! Either way a release that names another of the author's books, or a series
//! number other than this book's, is that other book.

use nats_jobs::{MediaWant, StructuralRejectReason};

use super::{any_title_similar, normalize_author_tokens};
use crate::parsing::{ParsedMedia, ParsedRelease};
use crate::watch::Release;
use crate::watch::types::ReleaseMedia;

struct Want<'a> {
    title: &'a str,
    author: Option<&'a str>,
    alternates: &'a [String],
    others: &'a [String],
    position: Option<&'a str>,
    year: Option<i32>,
}

impl<'a> Want<'a> {
    fn of(media_want: &'a MediaWant) -> Option<Self> {
        let (MediaWant::Book {
            title,
            author,
            alternate_titles,
            other_titles,
            series_position,
            release_date,
            ..
        }
        | MediaWant::Audiobook {
            title,
            author,
            alternate_titles,
            other_titles,
            series_position,
            release_date,
            ..
        }) = media_want
        else {
            return None;
        };
        Some(Self {
            title,
            author: author.as_deref().filter(|a| !a.trim().is_empty()),
            alternates: alternate_titles.as_deref().unwrap_or_default(),
            others: other_titles.as_deref().unwrap_or_default(),
            position: series_position.as_deref(),
            year: release_date
                .as_deref()
                .and_then(|d| d.get(..4))
                .and_then(|y| y.parse().ok()),
        })
    }

    fn position_number(&self) -> f64 {
        self.position
            .and_then(|p| p.trim().parse().ok())
            .unwrap_or(1.0)
    }

    fn titles(&self) -> impl Iterator<Item = &str> {
        std::iter::once(self.title).chain(self.alternates.iter().map(String::as_str))
    }
}

pub(super) fn reject(
    release: &Release,
    parsed: &ParsedRelease,
    media_want: &MediaWant,
) -> Option<StructuralRejectReason> {
    let want = Want::of(media_want)?;
    let (title, parsed_author, year, parsed_position) = match &parsed.media {
        ParsedMedia::Book(b) => (
            b.title.as_str(),
            b.author.as_deref(),
            b.year,
            b.series_position.as_deref(),
        ),
        ParsedMedia::Audiobook(a) => (
            a.title.as_str(),
            a.author.as_deref(),
            a.year,
            a.series_position.as_deref(),
        ),
        _ => return None,
    };
    let indexer_author = match &release.media {
        Some(ReleaseMedia::Book { author }) => author.as_deref(),
        _ => None,
    };
    if want.title.is_empty() || title.is_empty() {
        return None;
    }

    let stripped = match (want.author, parsed_author) {
        (Some(author), None) => strip_author_words(title, author),
        _ => None,
    };
    let mut author_named = stripped.is_some();
    if let Some(wanted) = want.author {
        for named in [parsed_author, indexer_author].into_iter().flatten() {
            match authors_overlap(wanted, named) {
                Some(false) => return Some(StructuralRejectReason::AuthorMismatch),
                Some(true) => author_named = true,
                None => {}
            }
        }
    }
    let candidate = stripped.unwrap_or_else(|| title.to_string());

    if let (Some(released), Some(published)) = (year, want.year) {
        let released = released as i32;
        // Reprints come later, never earlier. A name with no author has only
        // its year to tell a same-titled book apart, so it must be close.
        if released < published - 1 || (!author_named && released > published + 1) {
            return Some(StructuralRejectReason::YearMismatch);
        }
    }

    if !any_title_similar(want.title, Some(want.alternates), &candidate, !author_named)
        || names_other_book(&want, &candidate)
        || wrong_series_number(&want, &candidate)
        || parsed_position
            .and_then(|p| number(&p.trim().to_lowercase()))
            .is_some_and(|n| n != want.position_number())
    {
        return Some(StructuralRejectReason::TitleMismatch);
    }
    None
}

/// `None` when either side has no comparable words.
fn authors_overlap(wanted: &str, named: &str) -> Option<bool> {
    let wanted = normalize_author_tokens(wanted);
    let named = normalize_author_tokens(named);
    if wanted.is_empty() || named.is_empty() {
        return None;
    }
    Some(wanted.iter().any(|t| named.contains(t)))
}

fn tokens(s: &str) -> Vec<String> {
    kob_normalize::canonical(s)
        .split_whitespace()
        .map(String::from)
        .collect()
}

/// `Author.Name.Title` names have no separator for the parser to split on, so
/// the wanted author's words are dropped from either end. `None` when there
/// were none, or nothing else.
fn strip_author_words(title: &str, author: &str) -> Option<String> {
    let author_tokens = tokens(author);
    let tokens = tokens(title);
    let is_author = |t: &&String| author_tokens.contains(t);
    let start = tokens.iter().take_while(is_author).count();
    let end = tokens.len() - tokens[start..].iter().rev().take_while(is_author).count();
    (start < end && (start > 0 || end < tokens.len())).then(|| tokens[start..end].join(" "))
}

fn find_span(haystack: &[String], needle: &[String]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// The release is titled exactly as another of the author's books, or names
/// one inside it. A book whose title sits inside the wanted one ("Dune" in
/// "Dune Messiah") is only that book when it is the whole release title.
fn names_other_book(want: &Want, candidate: &str) -> bool {
    let candidate = tokens(candidate);
    let wanted: Vec<Vec<String>> = want.titles().map(tokens).collect();
    want.others.iter().map(|o| tokens(o)).any(|other| {
        if other.is_empty() || wanted.contains(&other) {
            return false;
        }
        if other == candidate {
            return true;
        }
        let inside_wanted = wanted.iter().any(|w| find_span(w, &other).is_some());
        !inside_wanted && find_span(&candidate, &other).is_some()
    })
}

const SERIES_MARKERS: &[&str] = &["book", "bk", "volume", "vol", "part", "no", "number"];
const NUMBER_WORDS: &[&str] = &[
    "one", "two", "three", "four", "five", "six", "seven", "eight", "nine", "ten",
];
const ROMAN: &[&str] = &["i", "ii", "iii", "iv", "v", "vi", "vii", "viii", "ix", "x"];

fn number(token: &str) -> Option<f64> {
    if let Ok(n) = token.parse::<f64>() {
        return Some(n);
    }
    NUMBER_WORDS
        .iter()
        .chain(ROMAN)
        .position(|w| *w == token)
        .map(|i| (i % 10 + 1) as f64)
}

/// "Dune 2", "Dune Book 2", "Dune Part Two" for book 1 of Dune. A standalone
/// book counts as number 1. The ebook parser lifts "Book 2" out of the title
/// into a series position, which `reject` compares the same way.
fn wrong_series_number(want: &Want, candidate: &str) -> bool {
    let candidate = tokens(candidate);
    let position = want.position_number();
    want.titles().map(tokens).any(|wanted| {
        let Some(start) = find_span(&candidate, &wanted) else {
            return false;
        };
        let mut after = candidate[start + wanted.len()..].iter();
        let next = match after.next() {
            Some(t) if SERIES_MARKERS.contains(&t.as_str()) => after.next(),
            other => other,
        };
        next.and_then(|t| number(t)).is_some_and(|n| n != position)
    })
}
