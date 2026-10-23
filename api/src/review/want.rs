use nats_jobs::{MediaWant, ParsedMedia};
use time::OffsetDateTime;

use crate::services::want::scoring::{ExplainedSlot, MatchExplanation, tokenize_sport};

pub fn want_label(want: &MediaWant) -> String {
    crate::media::search_terms_from_media_want(want)
        .into_iter()
        .next()
        .unwrap_or_else(|| crate::media::parse_media_kind(want).to_string())
}

fn parse_instant(s: &str) -> Option<OffsetDateTime> {
    crate::services::upgrade::parse_publish_date(s.trim())
}

/// When the want's release was due: the session start or event day for
/// sport, the air date for TV, the publication date for books. Movies have
/// no date that says a release should exist, so they are never missed.
pub fn due_at(want: &MediaWant) -> Option<OffsetDateTime> {
    match want {
        MediaWant::SportEvent {
            event_date, meta, ..
        } => meta
            .session_start
            .as_deref()
            .and_then(parse_instant)
            .or_else(|| event_date.as_deref().and_then(parse_instant)),
        MediaWant::TvEpisode { air_date, .. } => air_date.as_deref().and_then(parse_instant),
        MediaWant::TvSeason { last_air_date, .. } => {
            last_air_date.as_deref().and_then(parse_instant)
        }
        MediaWant::Book { release_date, .. } | MediaWant::Audiobook { release_date, .. } => {
            release_date.as_deref().and_then(parse_instant)
        }
        _ => None,
    }
}

fn episode_tag(season: &str, episode: Option<&str>) -> Option<String> {
    let s: u32 = season.parse().ok()?;
    match episode {
        Some(e) => Some(format!("S{s:02}E{:02}", e.parse::<u32>().ok()?)),
        None => Some(format!("S{s:02}")),
    }
}

/// The names a release for `want` would carry, to rank sightings by.
pub fn names(want: &MediaWant) -> Vec<String> {
    let with_alts = |name: &str, alts: &Option<Vec<String>>| -> Vec<String> {
        std::iter::once(name.to_string())
            .chain(alts.iter().flatten().cloned())
            .collect()
    };
    match want {
        MediaWant::TvEpisode {
            show_name,
            season,
            episode,
            alternate_titles,
            ..
        } => {
            let tag = episode_tag(season, Some(episode)).unwrap_or_default();
            with_alts(show_name, alternate_titles)
                .into_iter()
                .map(|n| format!("{n} {tag}").trim().to_string())
                .collect()
        }
        MediaWant::TvSeason {
            show_name,
            season,
            alternate_titles,
            ..
        } => {
            let tag = episode_tag(season, None).unwrap_or_default();
            with_alts(show_name, alternate_titles)
                .into_iter()
                .map(|n| format!("{n} {tag}").trim().to_string())
                .collect()
        }
        MediaWant::Movie {
            title,
            year,
            alternate_titles,
            ..
        } => with_alts(title, alternate_titles)
            .into_iter()
            .map(|n| match year {
                Some(y) => format!("{n} {y}"),
                None => n,
            })
            .collect(),
        MediaWant::Book {
            title,
            author,
            alternate_titles,
            ..
        }
        | MediaWant::Audiobook {
            title,
            author,
            alternate_titles,
            ..
        } => with_alts(title, alternate_titles)
            .into_iter()
            .flat_map(|n| {
                let by = author.as_deref().map(|a| format!("{a} {n}"));
                std::iter::once(n).chain(by)
            })
            .collect(),
        MediaWant::SportEvent {
            league_name,
            location,
            meta,
            ..
        } => {
            let leagues: Vec<&str> = std::iter::once(league_name.as_str())
                .chain(meta.class.as_deref())
                .chain(meta.league_aliases.iter().flatten().map(String::as_str))
                .collect();
            let places: Vec<&str> = [location, &meta.venue, &meta.city, &meta.country]
                .into_iter()
                .flatten()
                .map(String::as_str)
                .chain(meta.location_aliases.iter().flatten().map(String::as_str))
                .chain(meta.event_aliases.iter().flatten().map(String::as_str))
                .collect();
            leagues
                .iter()
                .flat_map(|l| places.iter().map(move |p| format!("{l} {p}")))
                .collect()
        }
        other => crate::media::search_terms_from_media_want(other),
    }
}

const COMMON_WORDS: &[&str] = &["the", "and", "season", "race", "grand", "prix", "round"];

/// Distinctive words of the want's names, plain alphanumerics so they are
/// safe in a text search query.
pub fn words(names: &[String]) -> Vec<String> {
    let mut words: Vec<String> = names
        .iter()
        .flat_map(|n| {
            n.to_lowercase()
                .split(|c: char| !c.is_alphanumeric())
                .filter(|w| w.chars().count() >= 3 && !COMMON_WORDS.contains(w))
                .map(String::from)
                .collect::<Vec<_>>()
        })
        .filter(|w| !w.chars().all(|c| c.is_ascii_digit()))
        .collect();
    words.sort();
    words.dedup();
    words
}

/// Years, dates and rounds the matcher reads before it looks for quality
/// tags, so a year is never mistaken for a resolution.
fn read_first(text: &str) -> bool {
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    text.starts_with("zd")
        || (text.len() == 4 && (text.starts_with("19") || text.starts_with("20")) && digits(text))
        || text
            .strip_prefix('r')
            .is_some_and(|d| d.len() <= 3 && digits(d))
}

/// Sport tokens before the first quality or group tag that the matcher did
/// not place: where a name it does not know would be.
pub fn slot_unexplained(title: &str, explanation: &MatchExplanation) -> Vec<String> {
    tokenize_sport(title)
        .into_iter()
        .take_while(|t| read_first(&t.text) || !(t.group || t.strong_quality))
        .filter(|t| explanation.unexplained.contains(&t.text))
        .map(|t| t.text)
        .collect()
}

/// A different class or season is a different event, however close the name.
pub fn contradicts_event(explanation: &MatchExplanation) -> bool {
    explanation
        .contradictions
        .iter()
        .any(|c| matches!(c.slot, ExplainedSlot::Class | ExplainedSlot::Season))
}

/// What the user most likely wants to remember for this release: the words
/// the sport matcher could not place, or the show or title the release names.
pub fn suggested_alias(
    want: &MediaWant,
    title: &str,
    explanation: Option<&MatchExplanation>,
) -> Option<String> {
    if let Some(ex) = explanation {
        let words = slot_unexplained(title, ex);
        return (!words.is_empty()).then(|| words.join(" "));
    }
    let parsed = crate::parsing::parse_release(title, crate::media::parse_media_kind(want));
    let name = match &parsed.media {
        ParsedMedia::Tv(tv) => tv.show_name.clone(),
        ParsedMedia::Movie(m) => m.title.clone(),
        ParsedMedia::Book(b) => b.title.clone(),
        ParsedMedia::Audiobook(a) => a.title.clone(),
        _ => return None,
    };
    let name = name.trim().to_string();
    let known = names(want)
        .iter()
        .any(|n| n.to_lowercase().starts_with(&name.to_lowercase()));
    (!name.is_empty() && !known).then_some(name)
}
