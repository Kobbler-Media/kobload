mod tokens;
mod vocab;

use std::collections::{HashMap, HashSet};

use nats_jobs::{MediaWant, StructuralRejectReason};
use time::Date;

use crate::parsing::{canonical_sport_text, country_for_code, country_names};
pub(crate) use tokens::{Token, tokenize};
use tokens::{normalize_phrase, parse_date_marker};
use vocab::{Phrase, Slot, UMBRELLA, class_of, closed_vocab, session_of};

const DATE_WINDOW_DAYS: i64 = 3;
const PUBLISH_SUPPORT_DAYS: i64 = 10;
const PENALTY_THRESHOLD: f64 = 1.5;
const MAIN_ZONE_COST: f64 = 0.5;
const NUMBER_COST: f64 = 0.25;
const TRAILER_COST: f64 = 0.1;
const TRAILER_CAP: f64 = 0.5;
const FULL_SCORE: f64 = 8.5;

#[derive(Debug)]
struct Place {
    name: String,
    canon: String,
    country: bool,
    short: bool,
}

#[derive(Debug)]
pub(crate) struct SportVocab {
    classes: HashSet<String>,
    umbrella: HashSet<String>,
    phrases: Vec<Phrase>,
    sessions: HashSet<String>,
    race_session: bool,
    places: Vec<Place>,
    country: Option<String>,
    named_after_country: bool,
    round: Option<u32>,
    season: Option<i32>,
    event_date: Option<Date>,
}

#[derive(Debug, Clone)]
pub(crate) struct SportVerdict {
    pub matched: bool,
    pub score: f64,
    pub reject: Option<StructuralRejectReason>,
    pub summary: String,
}

fn parse_date(s: &str) -> Option<Date> {
    let fmt = time::macros::format_description!("[year]-[month]-[day]");
    Date::parse(s.get(..10)?, &fmt).ok()
}

fn canon(s: &str) -> String {
    canonical_sport_text(country_for_code(s.trim()).unwrap_or(s))
}

impl SportVocab {
    /// `None` for a want without any of the alias fields, and for wants that
    /// are not a motorsport session this vocabulary knows (no place, no
    /// session, or a session it can't name), which keep the fuzzy targets.
    pub(crate) fn from_want(want: &MediaWant) -> Option<Self> {
        let MediaWant::SportEvent {
            league_name,
            season,
            round,
            location,
            session_type,
            event_title,
            event_date,
            meta,
            ..
        } = want
        else {
            return None;
        };
        let has_aliases = meta.location_aliases.is_some()
            || meta.event_aliases.is_some()
            || meta.league_aliases.is_some()
            || meta.venue.is_some()
            || meta.city.is_some()
            || meta.country_iso.is_some()
            || meta.class.is_some();
        let session_type = session_type.as_deref()?;
        if !has_aliases || location.is_none() && meta.location_aliases.is_none() {
            return None;
        }

        let league_names: Vec<&str> = std::iter::once(league_name.as_str())
            .chain(meta.class.as_deref())
            .chain(meta.league_aliases.iter().flatten().map(String::as_str))
            .collect();
        let class = league_names
            .iter()
            .find_map(|l| class_of(l))
            .unwrap_or_else(|| "league".to_string());
        let closed: HashMap<Vec<String>, &str> = closed_vocab()
            .iter()
            .filter(|p| p.slot == Slot::Class)
            .map(|p| (p.words.clone(), p.value.as_str()))
            .collect();
        let mut phrases: Vec<Phrase> = league_names
            .iter()
            .map(|l| normalize_phrase(l))
            .filter(|words| !words.is_empty() && closed.get(words).is_none_or(|v| *v == class))
            .map(|words| Phrase {
                words,
                slot: Slot::Class,
                value: class.clone(),
            })
            .collect();

        let session_words: HashSet<&str> = closed_vocab()
            .iter()
            .filter(|p| matches!(p.slot, Slot::Class | Slot::Session))
            .flat_map(|p| p.words.iter().map(String::as_str))
            .collect();
        phrases.extend(
            meta.event_aliases
                .iter()
                .flatten()
                .map(|e| normalize_phrase(e))
                .filter(|words| {
                    !words.is_empty()
                        && words.iter().all(|w| {
                            !session_words.contains(w.as_str())
                                && !w.bytes().any(|b| b.is_ascii_digit())
                        })
                })
                .map(|words| Phrase {
                    words,
                    slot: Slot::Event,
                    value: "event".to_string(),
                }),
        );

        let superpole = [location.as_deref(), event_title.as_deref()]
            .into_iter()
            .flatten()
            .any(|t| t.to_lowercase().contains("superpole"));
        let session = if session_type.eq_ignore_ascii_case("race") && superpole {
            "superpole_race".to_string()
        } else {
            session_of(session_type)?
        };

        let country = meta
            .country_iso
            .as_deref()
            .or(meta.country.as_deref())
            .map(canon);
        let place_names = location
            .iter()
            .chain(&meta.venue)
            .chain(&meta.city)
            .chain(&meta.region)
            .chain(&meta.country)
            .chain(&meta.country_iso)
            .chain(meta.location_aliases.iter().flatten());
        let mut seen = HashSet::new();
        let places = place_names
            .filter_map(|name| {
                let c = canon(name);
                (!c.is_empty() && seen.insert(name.trim().to_lowercase())).then(|| {
                    let short = name.trim().chars().count() <= 3;
                    Place {
                        country: country.as_deref() == Some(c.as_str()),
                        short,
                        name: name.clone(),
                        canon: c,
                    }
                })
            })
            .collect();

        Some(Self {
            umbrella: UMBRELLA
                .iter()
                .filter(|(child, _)| *child == class)
                .map(|(_, parent)| (*parent).to_string())
                .collect(),
            classes: HashSet::from([class]),
            phrases,
            race_session: session == "race",
            sessions: HashSet::from([session]),
            named_after_country: location.as_deref().map(canon) == country,
            places,
            country,
            round: round.filter(|r| *r > 0),
            season: season.trim().parse().ok(),
            event_date: event_date
                .as_deref()
                .and_then(parse_date)
                .or_else(|| meta.session_start.as_deref().and_then(parse_date)),
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
enum Assign {
    Phrase {
        slot: Slot,
        value: String,
        start: usize,
    },
    Date(Date),
    Year(i32),
    Round(u32),
    Place,
    Quality,
    Group,
}

fn is_year(t: &str) -> Option<i32> {
    (t.len() == 4 && (t.starts_with("19") || t.starts_with("20")))
        .then(|| t.parse().ok())
        .flatten()
}

fn round_token(t: &str) -> Option<u32> {
    let d = t.strip_prefix('r')?;
    (!d.is_empty() && d.len() <= 3 && d.bytes().all(|b| b.is_ascii_digit()))
        .then(|| d.parse().ok())
        .flatten()
}

fn longest_matches(tokens: &[Token], assign: &mut [Option<Assign>], dict: &[&Phrase]) {
    let mut by_first: HashMap<&str, Vec<&Phrase>> = HashMap::new();
    for p in dict {
        by_first.entry(p.words[0].as_str()).or_default().push(p);
    }
    let priority = |slot: Slot| match slot {
        Slot::Class | Slot::Session => 3,
        Slot::Event => 2,
        Slot::Broadcaster => 1,
        Slot::Filler => 0,
    };
    let mut i = 0;
    while i < tokens.len() {
        if assign[i].is_some() {
            i += 1;
            continue;
        }
        let best = by_first
            .get(tokens[i].text.as_str())
            .into_iter()
            .flatten()
            .filter(|p| {
                i + p.words.len() <= tokens.len()
                    && p.words
                        .iter()
                        .enumerate()
                        .all(|(k, w)| assign[i + k].is_none() && &tokens[i + k].text == w)
            })
            .max_by_key(|p| (p.words.len(), priority(p.slot)));
        match best {
            Some(p) => {
                for slot in &mut assign[i..i + p.words.len()] {
                    *slot = Some(Assign::Phrase {
                        slot: p.slot,
                        value: p.value.clone(),
                        start: i,
                    });
                }
                i += p.words.len();
            }
            None => i += 1,
        }
    }
}

#[derive(Default)]
struct Reading {
    class_hit: bool,
    other_class: bool,
    sessions_named: usize,
    session_hit: bool,
    other_session: bool,
    strong_place: bool,
    weak_place: bool,
    other_place: bool,
    round_hit: bool,
    other_round: bool,
    year_hit: bool,
    other_year: bool,
    date_hit: bool,
    other_date: bool,
    penalty: f64,
    notes: Vec<String>,
}

fn names_a_country(words: &[&str], country: Option<&str>) -> Option<String> {
    let countries: HashSet<String> = country_names().map(canonical_sport_text).collect();
    (0..words.len()).find_map(|i| {
        [1, 2].into_iter().find_map(|len| {
            let w = words.get(i..i + len)?.join(" ");
            (countries.contains(&w) && country != Some(w.as_str())).then_some(w)
        })
    })
}

pub(crate) fn judge(
    vocab: &SportVocab,
    tokens: &[Token],
    size: i64,
    resolution: Option<&str>,
    published: Option<Date>,
) -> SportVerdict {
    let n = tokens.len();
    let mut assign: Vec<Option<Assign>> = vec![None; n];
    for (i, t) in tokens.iter().enumerate() {
        assign[i] = if let Some(d) = parse_date_marker(&t.text) {
            Some(Assign::Date(d))
        } else if let Some(y) = is_year(&t.text) {
            Some(Assign::Year(y))
        } else {
            round_token(&t.text).map(Assign::Round)
        };
    }
    for i in 1..n {
        let t = &tokens[i].text;
        if assign[i].is_none()
            && matches!(assign[i - 1], Some(Assign::Year(_)))
            && t.len() <= 2
            && t.bytes().all(|b| b.is_ascii_digit())
        {
            assign[i] = t.parse().ok().map(Assign::Round);
        }
    }
    let mut dict: Vec<&Phrase> = vocab.phrases.iter().collect();
    dict.extend(closed_vocab());
    longest_matches(tokens, &mut assign, &dict);
    let first_quality = (0..n)
        .find(|&i| assign[i].is_none() && (tokens[i].group || tokens[i].strong_quality))
        .unwrap_or(n);
    // Before the first strong tag a weak one ("Czech", "German": audio
    // languages) is as likely the place, so it waits for the place reading.
    for (i, (a, t)) in assign.iter_mut().zip(tokens).enumerate() {
        if a.is_none() {
            if t.group {
                *a = Some(Assign::Group);
            } else if t.quality && (t.strong_quality || i >= first_quality) {
                *a = Some(Assign::Quality);
            }
        }
    }

    let mut r = Reading::default();
    let mut hits: Vec<String> = Vec::new();
    let mut seen_starts = HashSet::new();
    for (i, a) in assign.iter().enumerate() {
        match a {
            Some(Assign::Phrase { slot, value, start }) => {
                if !seen_starts.insert(*start) {
                    continue;
                }
                match slot {
                    Slot::Class if vocab.classes.contains(value) => {
                        r.class_hit = true;
                        hits.push(format!("class={value}"));
                    }
                    Slot::Class if vocab.umbrella.contains(value) => {}
                    Slot::Class => {
                        r.other_class = true;
                        r.notes.push(format!("class {value}"));
                    }
                    Slot::Session => {
                        r.sessions_named += 1;
                        if vocab.sessions.contains(value) {
                            r.session_hit = true;
                            hits.push(format!("session={value}"));
                        } else {
                            r.other_session = true;
                            r.notes.push(format!("session {value}"));
                        }
                    }
                    Slot::Event => {
                        r.strong_place = true;
                        hits.push(format!("event={}", tokens[i].text));
                    }
                    Slot::Filler | Slot::Broadcaster => {}
                }
            }
            Some(Assign::Date(d)) => match vocab.event_date {
                Some(ev) if (*d - ev).whole_days().abs() <= DATE_WINDOW_DAYS => {
                    r.date_hit = true;
                    hits.push(format!("date={d}"));
                }
                Some(_) => {
                    r.other_date = true;
                    r.notes.push(format!("date {d}"));
                }
                None => {}
            },
            Some(Assign::Year(y)) => match vocab.season {
                Some(s) if s == *y => r.year_hit = true,
                Some(_) => {
                    r.other_year = true;
                    r.notes.push(format!("season {y}"));
                }
                None => {}
            },
            Some(Assign::Round(n)) => match vocab.round {
                Some(w) if w == *n => {
                    r.round_hit = true;
                    hits.push(format!("round={n}"));
                }
                Some(_) if *n > 0 => {
                    r.other_round = true;
                    r.notes.push(format!("round {n}"));
                }
                _ => {}
            },
            _ => {}
        }
    }

    read_places(vocab, tokens, &mut assign, first_quality, &mut r, &mut hits);
    for (a, t) in assign.iter_mut().zip(tokens) {
        if a.is_none() && t.quality {
            *a = Some(Assign::Quality);
        }
    }

    let mut trailer = 0.0;
    for (i, t) in tokens.iter().enumerate() {
        if assign[i].is_some() {
            continue;
        }
        if i >= first_quality {
            let cost = TRAILER_COST.min(TRAILER_CAP - trailer).max(0.0);
            trailer += cost;
            r.penalty += cost;
        } else if t.text.bytes().all(|b| b.is_ascii_digit()) {
            r.penalty += NUMBER_COST;
        } else {
            r.penalty += MAIN_ZONE_COST;
        }
    }

    let publish_support = matches!((published, vocab.event_date),
        (Some(p), Some(ev)) if (-1..=PUBLISH_SUPPORT_DAYS).contains(&(p - ev).whole_days()));
    let anchored = r.strong_place || r.weak_place || r.round_hit || r.date_hit;
    let matched = r.class_hit
        && !r.other_class
        && !r.other_date
        && !r.other_year
        && anchored
        && r.penalty <= PENALTY_THRESHOLD;

    let placed = r.strong_place || r.round_hit || r.date_hit || r.weak_place && publish_support;
    let pack = crate::services::want::scoring::is_sport_weekend_pack(size, resolution);
    let implicit_race = r.sessions_named == 0 && vocab.race_session && !pack;
    let reject = if r.other_year {
        Some(StructuralRejectReason::SeasonMismatch)
    } else if r.other_round {
        Some(StructuralRejectReason::RoundMismatch)
    } else if r.other_place || !placed {
        Some(StructuralRejectReason::LocationMismatch)
    } else if r.other_session {
        Some(StructuralRejectReason::SessionMismatch)
    } else if !r.session_hit && !implicit_race {
        Some(StructuralRejectReason::SessionMissing)
    } else {
        None
    };

    let score = 2.0 * f64::from(u8::from(r.class_hit))
        + if r.strong_place {
            2.0
        } else if r.weak_place {
            1.0
        } else {
            0.0
        }
        + f64::from(u8::from(r.round_hit))
        + 2.0 * f64::from(u8::from(r.session_hit || implicit_race))
        + 0.5 * f64::from(u8::from(r.year_hit))
        + f64::from(u8::from(r.date_hit))
        - r.penalty;
    SportVerdict {
        matched,
        score: (score / FULL_SCORE).clamp(0.0, 1.0),
        reject,
        summary: format!(
            "sport [{}]{}",
            hits.join(" "),
            if r.notes.is_empty() {
                String::new()
            } else {
                format!(" against [{}]", r.notes.join(", "))
            }
        ),
    }
}

/// A run is the want's place when any one of its place names passes
/// the location gate on its own: joined, "Emilia Romagna Misano" would no
/// longer word-match `Italy.Misano`. Two- and three-letter names (country
/// codes) only count right after the class, season or round, where releases
/// put the place; elsewhere the same letters are language tags.
fn read_places(
    vocab: &SportVocab,
    tokens: &[Token],
    assign: &mut [Option<Assign>],
    first_quality: usize,
    r: &mut Reading,
    hits: &mut Vec<String>,
) {
    let mut i = 0;
    while i < first_quality {
        if assign[i].is_some() {
            i += 1;
            continue;
        }
        let start = i;
        while i < first_quality && assign[i].is_none() {
            i += 1;
        }
        let words: Vec<&str> = tokens[start..i].iter().map(|t| t.text.as_str()).collect();
        let span = words.join(" ");
        let in_place_position = start > 0
            && matches!(
                assign[start - 1],
                Some(
                    Assign::Year(_)
                        | Assign::Round(_)
                        | Assign::Date(_)
                        | Assign::Phrase {
                            slot: Slot::Class,
                            ..
                        }
                )
            );
        let short_span = words.len() == 1 && span.chars().count() <= 3;
        let found = vocab
            .places
            .iter()
            .filter(|p| !(p.short || short_span) || in_place_position)
            .filter(|p| crate::services::want::scoring::location_contains(&span, &p.name))
            .map(|p| place_is_weak(vocab, p, &span))
            .min();
        match found {
            Some(weak) => {
                if weak {
                    r.weak_place = true;
                } else {
                    r.strong_place = true;
                }
                hits.push(format!(
                    "place={span}({})",
                    if weak { "weak" } else { "strong" }
                ));
                for a in &mut assign[start..i] {
                    *a = Some(Assign::Place);
                }
            }
            None => {
                if let Some(other) = names_a_country(&words, vocab.country.as_deref()) {
                    r.other_place = true;
                    r.notes.push(format!("place {other}"));
                    for a in &mut assign[start..i] {
                        *a = Some(Assign::Place);
                    }
                }
            }
        }
    }
}

/// A country or a code names the round only when the round is named after
/// the country (TheSportsDB's "Germany - Race", scene's `MotoGP.2026.Germany`)
/// and the release says nothing more. Otherwise it is one of the country's
/// rounds and needs the round, the date or the publish date to say which:
/// Spain hosts four.
fn place_is_weak(vocab: &SportVocab, place: &Place, span: &str) -> bool {
    place.short
        || place.country
            && !(vocab.named_after_country && canonical_sport_text(span) == place.canon)
}

pub(crate) fn publish_date(s: &str) -> Option<Date> {
    use time::format_description::well_known::{Rfc2822, Rfc3339};
    time::OffsetDateTime::parse(s, &Rfc3339)
        .or_else(|_| time::OffsetDateTime::parse(s, &Rfc2822))
        .map(|dt| dt.date())
        .ok()
        .or_else(|| parse_date(s))
}
