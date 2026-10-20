//! Sport event release name parsing.
//!
//! Handles common sport release naming conventions:
//! - Round+Location+Session: `MotoGP.2024.R01.Thailand.Race.1080p`
//! - Round+Location: `Formula.1.2024.R05.Monaco.1080p`
//! - Year+Location+Session: `MotoGP.2024.Thailand.Race.1080p`
//! - Year+Location: `MotoGP.2024.Thailand.1080p`
//! - YEARxROUND: `MotoGP.2024x02.Portugal.Race`
//! - UFC/Event number: `UFC.302.Main.Card.1080p`
//! - Teams format: `Premier.League.2024.Arsenal.vs.Chelsea.1080p`
//! - Season pack: `MotoGP.2024.Season.Complete`

use regex::Regex;
use std::sync::LazyLock;

use super::common::{clean_title, extract_year};
use super::types::{ParsedMedia, SportParsed};

// =============================================================================
// Regexes
// =============================================================================

/// Session alternatives shared by ROUND_LOC_SESSION_RE and YEAR_LOC_SESSION_RE.
/// Longer/more specific variants are listed first (alternation is left-to-right greedy).
/// "Pre[-.\s]?Event[.\s]Press[.\s]Conference" etc. catch motorsport non-race sessions
/// that regularly appear in release titles but aren't the main event.
const SESSION_ALTERNATIVES: &str = r"Sprint[\.\s]Race|Sprint[\.\s]Qualifying|Sprint[\.\s]Shootout|Superpole[\.\s]Race|Free[\.\s]Practice[\.\s](?:One|Two|Three|Four|[1-4])|Free[\.\s]Practice|Qualifying[\.\s](?:One|Two|[12])|Race[\.\s](?:One|Two|[12])|Warm[\.\s]Up|Pre[-\.\s]Event[\.\s]Press[\.\s]Conference|Post[-\.\s]Event[\.\s]Press[\.\s]Conference|Press[\.\s]Conference|Media[\.\s]Day|Gear[\.\s]Up|Shakedown|Morning[\.\s]Session|Afternoon[\.\s]Session|Superpole|Sprint|Race|Qualifying|Practice[\.\s](?:One|Two|Three|Four|[1-4])|Practice|FP[1-4]|Q[12]";

/// Location character class — words of `[A-Z][A-Za-z-]+`.
/// Hyphens matter for titles like `Pre-Event` (session modifier) and hyphenated
/// place names (e.g. `Le-Mans`, though this is rare in release conventions).
/// Trailing hyphens are permitted by the class; in practice this is fine because
/// the location always terminates at a `[.\s]` separator or quality token.
const LOCATION_CLASS: &str = r"[A-Z][A-Za-z-]+(?:[\.\s][A-Z][A-Za-z-]+)*";
const LOCATION_CLASS_LAZY: &str = r"[A-Z][A-Za-z-]+(?:[\.\s][A-Z][A-Za-z-]+)*?";

/// Round+Location+Session: MotoGP.2024.R01.Thailand.Race.1080p
static ROUND_LOC_SESSION_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?ix)
        ^(?P<league>.+?)[\.\s]
        (?P<year>\d{{4}})[\.\s]
        (?:R|Round[\.\s]?)(?P<round>\d{{1,2}})[\.\s]
        (?P<location>{LOCATION_CLASS_LAZY})[\.\s]
        (?P<session>{SESSION_ALTERNATIVES})
        (?:[\.\s]|$)"
    ))
    .unwrap()
});

/// Round+Location (no session): Formula.1.2024.R05.Monaco.1080p
static ROUND_LOC_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?ix)
        ^(?P<league>.+?)[\.\s]
        (?P<year>\d{{4}})[\.\s]
        (?:R|Round[\.\s]?)(?P<round>\d{{1,2}})[\.\s]
        (?P<location>{LOCATION_CLASS})
        (?:[\.\s](?:\d{{3,4}}p|WEB|BluRay|HDTV|x26|hevc|av1)|$)"
    ))
    .unwrap()
});

/// Year+Location+Session (no round): MotoGP.2024.Thailand.Race.1080p
static YEAR_LOC_SESSION_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?ix)
        ^(?P<league>.+?)[\.\s]
        (?P<year>\d{{4}})[\.\s]
        (?P<location>{LOCATION_CLASS_LAZY})[\.\s]
        (?P<session>{SESSION_ALTERNATIVES})
        (?:[\.\s]|$)"
    ))
    .unwrap()
});

/// Year+Location (no round, no session): MotoGP.2024.Thailand.1080p
static YEAR_LOC_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(&format!(
        r"(?ix)
        ^(?P<league>.+?)[\.\s]
        (?P<year>\d{{4}})[\.\s]
        (?P<location>{LOCATION_CLASS})
        (?:[\.\s](?:\d{{3,4}}p|WEB|BluRay|HDTV|x26|hevc|av1)|$)"
    ))
    .unwrap()
});

/// YEARxROUND format: MotoGP.2024x02.Portugal.Race
static YEAR_X_ROUND_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?ix)
        ^(?P<league>.+?)[\.\s]
        (?P<year>\d{4})x(?P<round>\d{2,3})[\.\s]
        (?P<rest>.+?)
        (?:[\.\s](?:\d{3,4}p|WEB|BluRay|HDTV|x26|hevc|av1)|$)",
    )
    .unwrap()
});

/// UFC/numbered event with session info: UFC.302.Main.Card.1080p
static EVENT_NUMBER_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?ix)
        ^(?P<league>UFC|Bellator|ONE[\.\s]Championship|PFL|BKFC)[\.\s]
        (?P<number>\d{1,4})[\.\s]
        (?P<rest>.+?)
        (?:[\.\s](?:\d{3,4}p|WEB|BluRay|HDTV|x26|hevc|av1)|$)",
    )
    .unwrap()
});

/// UFC/numbered event without session info: UFC.302.1080p
static EVENT_NUMBER_BARE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?ix)
        ^(?P<league>UFC|Bellator|ONE[\.\s]Championship|PFL|BKFC)[\.\s]
        (?P<number>\d{1,4})
        (?:[\.\s](?:\d{3,4}p|WEB|BluRay|HDTV|x26|hevc|av1)|$)",
    )
    .unwrap()
});

/// Teams format: Premier.League.2024.Arsenal.vs.Chelsea.1080p
static TEAMS_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?ix)
        ^(?P<league>.+?)[\.\s]
        (?P<year>\d{4})[\.\s]
        (?P<team1>.+?)[\.\s]
        (?:vs?[\.\s])
        (?P<team2>.+?)
        (?:[\.\s](?:\d{3,4}p|WEB|BluRay|HDTV|x26|hevc|av1)|$)",
    )
    .unwrap()
});

/// Season pack: MotoGP.2024.Season.Complete or MotoGP.2024.Complete.Season
static SEASON_PACK_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?ix)
        ^(?P<league>.+?)[\.\s]
        (?P<year>\d{4})[\.\s]
        (?:Season[\.\s](?:Complete|Pack|Full)|Complete[\.\s]Season|Full[\.\s]Season)
        (?:[\.\s]|$)",
    )
    .unwrap()
});

// =============================================================================
// Session type normalization
// =============================================================================

fn normalize_session(session: &str) -> String {
    // Handle both `.` and `-` as word separators — "Pre-Event.Press.Conference"
    // from the release title should collapse to "pre event press conference"
    // before matching below.
    let s = session.replace(['.', '-'], " ");
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    match s.to_lowercase().as_str() {
        "fp1" | "free practice 1" | "free practice one" | "practice 1" | "practice one" => {
            "Free Practice 1".to_string()
        }
        "fp2" | "free practice 2" | "free practice two" | "practice 2" | "practice two" => {
            "Free Practice 2".to_string()
        }
        "fp3" | "free practice 3" | "free practice three" | "practice 3" | "practice three" => {
            "Free Practice 3".to_string()
        }
        "fp4" | "free practice 4" | "free practice four" | "practice 4" | "practice four" => {
            "Free Practice 4".to_string()
        }
        "free practice" => "Free Practice".to_string(),
        "q1" | "qualifying 1" | "qualifying one" => "Qualifying 1".to_string(),
        "q2" | "qualifying 2" | "qualifying two" => "Qualifying 2".to_string(),
        "race 1" | "race one" => "Race 1".to_string(),
        "race 2" | "race two" => "Race 2".to_string(),
        "superpole race" => "Superpole Race".to_string(),
        "superpole" => "Superpole".to_string(),
        "sprint race" | "sprint" => "Sprint Race".to_string(),
        "sprint qualifying" | "sprint shootout" => "Sprint Qualifying".to_string(),
        "warm up" => "Warm Up".to_string(),
        "press conference" | "pre event press conference" => "Press Conference".to_string(),
        "post event press conference" => "Post-Event Press Conference".to_string(),
        "media day" => "Media Day".to_string(),
        "gear up" => "Gear Up".to_string(),
        "shakedown" => "Shakedown".to_string(),
        "morning session" => "Morning Session".to_string(),
        "afternoon session" => "Afternoon Session".to_string(),
        _ => {
            // Capitalize first letter
            let mut chars = s.chars();
            match chars.next() {
                None => s,
                Some(c) => c.to_uppercase().to_string() + chars.as_str(),
            }
        }
    }
}

// =============================================================================
// Canonical sport text
// =============================================================================

/// Rewrite the spellings that differ between a sport want and a scene release
/// into one form, so the two compare token for token.
///
/// Wants are built from TheSportsDB ("SBK 2026 R12 Spanish ... Race 1") while
/// releases follow scene habit ("WSBK.2026.Round12.Spain.Jerez.Race.One"), and
/// without this the round, the session number and the place are all misses in
/// the fuzzy match: a WSBK race scored ~0.62 against its own want and was never
/// grabbed. Output is `kob_normalize::canonical` text with:
///
/// - rounds as `r12` ("Round12", "Round 12", "R12", "R012")
/// - numbered sessions as `race 1`, `fp1`, `q1` ("Race One", "Free Practice 1",
///   "Practice.One", "FP1", "Qualifying Two"), and "Warm Up" as `warmup`
/// - nationalities and alternative country names as one country
///   ("Spanish" → `spain`, "Great Britain"/"British"/"GB" → `uk`,
///   "Czech Republic"/"Czechia" → `czech`)
/// - league spellings as one name ("WSBK"/"WorldSBK" → `sbk`,
///   "Formula1"/"Formula 1" → `f1`)
pub fn canonical_sport_text(s: &str) -> String {
    let canon = kob_normalize::canonical(s);
    let words: Vec<&str> = canon.split_whitespace().collect();
    let mut out: Vec<String> = Vec::with_capacity(words.len());
    let mut i = 0;
    while i < words.len() {
        let w = words[i];
        let next = words.get(i + 1).copied();
        let next_num = next.and_then(session_ordinal);

        if let Some(n) = next
            && let Some(alias) = two_word_alias(w, n)
        {
            out.push(alias.to_string());
            i += 2;
            continue;
        }
        if matches!(w, "round" | "rd")
            && let Some(n) = next.and_then(|n| n.parse::<u32>().ok())
        {
            out.push(format!("r{n}"));
            i += 2;
            continue;
        }
        if let Some(n) = glued_round(w) {
            out.push(format!("r{n}"));
            i += 1;
            continue;
        }
        if w == "free"
            && next == Some("practice")
            && let Some(n) = words.get(i + 2).copied().and_then(session_ordinal)
        {
            out.push(format!("fp{n}"));
            i += 3;
            continue;
        }
        match (w, next_num) {
            ("practice" | "fp", Some(n)) => {
                out.push(format!("fp{n}"));
                i += 2;
            }
            ("qualifying" | "q", Some(n @ (1 | 2))) => {
                out.push(format!("q{n}"));
                i += 2;
            }
            ("race", Some(n @ (1 | 2))) => {
                out.push("race".to_string());
                out.push(n.to_string());
                i += 2;
            }
            ("warm", _) if next == Some("up") => {
                out.push("warmup".to_string());
                i += 2;
            }
            _ => {
                out.push(one_word_alias(w).unwrap_or(w).to_string());
                i += 1;
            }
        }
    }
    out.join(" ")
}

fn session_ordinal(w: &str) -> Option<u32> {
    match w {
        "1" | "one" => Some(1),
        "2" | "two" => Some(2),
        "3" | "three" => Some(3),
        "4" | "four" => Some(4),
        _ => None,
    }
}

/// "round12" / "r12" → 12. Only all-digit suffixes, so words like "rd" or
/// "r2d2" are left alone.
fn glued_round(w: &str) -> Option<u32> {
    let digits = w
        .strip_prefix("round")
        .or_else(|| w.strip_prefix('r'))
        .filter(|d| !d.is_empty() && d.len() <= 3 && d.bytes().all(|b| b.is_ascii_digit()))?;
    digits.parse().ok()
}

fn two_word_alias(a: &str, b: &str) -> Option<&'static str> {
    match (a, b) {
        ("great", "britain") | ("united", "kingdom") => Some("uk"),
        ("czech", "republic") => Some("czech"),
        ("formula", "1" | "one") => Some("f1"),
        _ => None,
    }
}

fn one_word_alias(w: &str) -> Option<&'static str> {
    Some(match w {
        // Not "superbike": British Superbikes is a different championship.
        "wsbk" | "worldsbk" => "sbk",
        "formula1" => "f1",
        "british" | "britain" | "gb" => "uk",
        "czechia" => "czech",
        "spanish" => "spain",
        "italian" => "italy",
        "french" => "france",
        "german" => "germany",
        "dutch" => "netherlands",
        "portuguese" => "portugal",
        "hungarian" => "hungary",
        "australian" => "australia",
        "austrian" => "austria",
        "japanese" => "japan",
        "brazilian" => "brazil",
        "belgian" => "belgium",
        "canadian" => "canada",
        "mexican" => "mexico",
        "chinese" => "china",
        "indonesian" => "indonesia",
        "malaysian" => "malaysia",
        "thai" => "thailand",
        "qatari" => "qatar",
        "argentine" | "argentinian" => "argentina",
        "finnish" => "finland",
        "turkish" => "turkey",
        _ => return None,
    })
}

// =============================================================================
// Parsing
// =============================================================================

/// Simple hyphen-separated: "Moto2 - R01 - Thailand Grand Prix Moto2.mkv"
/// Format: {league} - R{round} - {event_title}
static SIMPLE_ROUND_EVENT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?ix)
        ^(?P<league>.+?)\s+-\s+
        R(?P<round>\d{1,2})\s+-\s+
        (?P<event>.+?)
        (?:\.\w{2,4})?$",
    )
    .unwrap()
});

/// Parse a sport release name.
pub fn parse_sport(name: &str) -> (String, ParsedMedia, f64) {
    // Try simple hyphen-separated format: "League - R01 - Event Title"
    if let Some(result) = try_simple_round_event(name) {
        return result;
    }

    // SABnzbd / DrunkenSlug NZB names (and some scene torrents) use `_` as
    // a token separator instead of `.`. The regexes below only accept
    // `[\.\s]` so without this every underscore release falls through to
    // parse_fallback and structural_reject can't see the session_type.
    let normalized;
    let name = if name.contains('_') {
        normalized = name.replace('_', ".");
        normalized.as_str()
    } else {
        name
    };

    // Try most specific patterns first

    // 1. Round+Location+Session
    if let Some(result) = try_round_loc_session(name) {
        return result;
    }

    // 2. Round+Location (no session)
    if let Some(result) = try_round_loc(name) {
        return result;
    }

    // 3. YEARxROUND format
    if let Some(result) = try_year_x_round(name) {
        return result;
    }

    // 4. Season pack (must come before year+location patterns to avoid
    //    "Season" being matched as a location)
    if let Some(result) = try_season_pack(name) {
        return result;
    }

    // 5. Year+Location+Session (no round)
    if let Some(result) = try_year_loc_session(name) {
        return result;
    }

    // 6. UFC/Event number format
    if let Some(result) = try_event_number(name) {
        return result;
    }

    // 7. Teams format
    if let Some(result) = try_teams(name) {
        return result;
    }

    // 8. Year+Location (no round, no session)
    if let Some(result) = try_year_loc(name) {
        return result;
    }

    // 9. Fallback: extract league name + year
    parse_fallback(name)
}

fn try_simple_round_event(name: &str) -> Option<(String, ParsedMedia, f64)> {
    let caps = SIMPLE_ROUND_EVENT_RE.captures(name)?;
    let league = caps["league"].trim().to_string();
    let round: u32 = caps["round"].parse().ok()?;
    let event = caps["event"].trim().to_string();

    // Try to extract session type from the end of the event title
    let event_lower = event.to_lowercase();
    let (event_title, session) = extract_session_from_event(&event_lower, &event);

    let title = if let Some(ref sess) = session {
        format!("{league} R{round:02} {event_title} {sess}")
    } else {
        format!("{league} R{round:02} {event_title}")
    };

    let parsed = SportParsed {
        league_name: league,
        season: None, // filled from folder context
        round: Some(round),
        location: None,
        session_type: session,
        event_title: Some(event_title),
        is_pack: false,
        year: None,
    };

    Some((title, ParsedMedia::Sport(parsed), 0.90))
}

/// Try to split a session type off the end of an event title.
/// e.g. "Thailand Grand Prix Race" → ("Thailand Grand Prix", Some("Race"))
/// e.g. "Thailand Grand Prix Moto2" → ("Thailand Grand Prix Moto2", None)
fn extract_session_from_event(event_lower: &str, event_orig: &str) -> (String, Option<String>) {
    // Check for known session suffixes (longest first to avoid partial matches)
    let session_suffixes = [
        "superpole race",
        "sprint race",
        "sprint qualifying",
        "sprint shootout",
        "free practice one",
        "free practice two",
        "free practice three",
        "free practice four",
        "free practice 1",
        "free practice 2",
        "free practice 3",
        "free practice 4",
        "free practice",
        "qualifying one",
        "qualifying two",
        "qualifying 1",
        "qualifying 2",
        "qualifying",
        "race one",
        "race two",
        "race 1",
        "race 2",
        "warm up",
        "practice one",
        "practice two",
        "practice three",
        "practice four",
        "practice",
        "superpole",
        "sprint",
        "race",
        "fp1",
        "fp2",
        "fp3",
        "fp4",
        "q1",
        "q2",
    ];

    for suffix in session_suffixes {
        if event_lower.ends_with(suffix) {
            let prefix_len = event_orig.len() - suffix.len();
            let prefix = event_orig[..prefix_len].trim();
            if !prefix.is_empty() {
                return (prefix.to_string(), Some(normalize_session(suffix)));
            }
        }
    }

    (event_orig.to_string(), None)
}

fn clean_league(s: &str) -> String {
    s.replace('.', " ").trim().to_string()
}

fn clean_location(s: &str) -> String {
    s.replace('.', " ").trim().to_string()
}

fn try_round_loc_session(name: &str) -> Option<(String, ParsedMedia, f64)> {
    let caps = ROUND_LOC_SESSION_RE.captures(name)?;
    let league = clean_league(&caps["league"]);
    let year: u32 = caps["year"].parse().ok()?;
    let round: u32 = caps["round"].parse().ok()?;
    let location = clean_location(&caps["location"]);
    let session = normalize_session(&caps["session"]);

    let title = format!("{league} {year} R{round:02} {location} {session}");
    let parsed = SportParsed {
        league_name: league,
        season: Some(year),
        round: Some(round),
        location: Some(location),
        session_type: Some(session),
        event_title: None,
        is_pack: false,
        year: Some(year),
    };

    Some((title, ParsedMedia::Sport(parsed), 0.95))
}

fn try_round_loc(name: &str) -> Option<(String, ParsedMedia, f64)> {
    let caps = ROUND_LOC_RE.captures(name)?;
    let league = clean_league(&caps["league"]);
    let year: u32 = caps["year"].parse().ok()?;
    let round: u32 = caps["round"].parse().ok()?;
    let location = clean_location(&caps["location"]);

    let title = format!("{league} {year} R{round:02} {location}");
    let parsed = SportParsed {
        league_name: league,
        season: Some(year),
        round: Some(round),
        location: Some(location),
        session_type: None,
        event_title: None,
        is_pack: false,
        year: Some(year),
    };

    Some((title, ParsedMedia::Sport(parsed), 0.90))
}

fn try_year_loc_session(name: &str) -> Option<(String, ParsedMedia, f64)> {
    let caps = YEAR_LOC_SESSION_RE.captures(name)?;
    let league = clean_league(&caps["league"]);
    let year: u32 = caps["year"].parse().ok()?;
    let location = clean_location(&caps["location"]);
    let session = normalize_session(&caps["session"]);

    let title = format!("{league} {year} {location} {session}");
    let parsed = SportParsed {
        league_name: league,
        season: Some(year),
        round: None,
        location: Some(location),
        session_type: Some(session),
        event_title: None,
        is_pack: false,
        year: Some(year),
    };

    Some((title, ParsedMedia::Sport(parsed), 0.85))
}

fn try_year_loc(name: &str) -> Option<(String, ParsedMedia, f64)> {
    let caps = YEAR_LOC_RE.captures(name)?;
    let league = clean_league(&caps["league"]);
    let year: u32 = caps["year"].parse().ok()?;
    let location = clean_location(&caps["location"]);

    let title = format!("{league} {year} {location}");
    let parsed = SportParsed {
        league_name: league,
        season: Some(year),
        round: None,
        location: Some(location),
        session_type: None,
        event_title: None,
        is_pack: false,
        year: Some(year),
    };

    Some((title, ParsedMedia::Sport(parsed), 0.75))
}

fn try_year_x_round(name: &str) -> Option<(String, ParsedMedia, f64)> {
    let caps = YEAR_X_ROUND_RE.captures(name)?;
    let league = clean_league(&caps["league"]);
    let year: u32 = caps["year"].parse().ok()?;
    let round: u32 = caps["round"].parse().ok()?;
    let rest = caps["rest"].replace('.', " ");

    // Try to extract session type from the rest
    let rest_lower = rest.to_lowercase();
    let (location, session) = if let Some(pos) = rest_lower
        .find("race")
        .or_else(|| rest_lower.find("qualifying"))
        .or_else(|| rest_lower.find("sprint"))
        .or_else(|| rest_lower.find("practice"))
        .or_else(|| rest_lower.find("warm up"))
    {
        let loc = rest[..pos].trim().to_string();
        let sess = rest[pos..].trim().to_string();
        (
            if loc.is_empty() { None } else { Some(loc) },
            Some(normalize_session(&sess)),
        )
    } else {
        (Some(rest.trim().to_string()), None)
    };

    let title = if let Some(ref loc) = location {
        if let Some(ref sess) = session {
            format!("{league} {year} R{round:02} {loc} {sess}")
        } else {
            format!("{league} {year} R{round:02} {loc}")
        }
    } else if let Some(ref sess) = session {
        format!("{league} {year} R{round:02} {sess}")
    } else {
        format!("{league} {year} R{round:02}")
    };

    let parsed = SportParsed {
        league_name: league,
        season: Some(year),
        round: Some(round),
        location,
        session_type: session,
        event_title: None,
        is_pack: false,
        year: Some(year),
    };

    Some((title, ParsedMedia::Sport(parsed), 0.90))
}

fn try_event_number(name: &str) -> Option<(String, ParsedMedia, f64)> {
    // Try with session info first (e.g. "UFC.302.Main.Card.1080p")
    if let Some(caps) = EVENT_NUMBER_RE.captures(name) {
        let league = clean_league(&caps["league"]);
        let number: u32 = caps["number"].parse().ok()?;
        let rest = caps["rest"].replace('.', " ").trim().to_string();
        let rest_is_quality = rest.split_whitespace().next().is_some_and(|first| {
            let f = first.to_lowercase();
            f.ends_with('p') && f[..f.len() - 1].parse::<u32>().is_ok()
                || matches!(f.as_str(), "web" | "bluray" | "hdtv" | "hevc" | "av1")
                || f.starts_with("x26")
        });

        if !rest.is_empty() && !rest_is_quality {
            let event_title = format!("{league} {number} {rest}");
            let title = event_title.clone();
            let parsed = SportParsed {
                league_name: league,
                season: None,
                round: Some(number),
                location: None,
                session_type: Some(rest),
                event_title: Some(event_title),
                is_pack: false,
                year: None,
            };
            return Some((title, ParsedMedia::Sport(parsed), 0.85));
        }
    }

    // Try bare format (e.g. "UFC.302.1080p" — no session info)
    let caps = EVENT_NUMBER_BARE_RE.captures(name)?;
    let league = clean_league(&caps["league"]);
    let number: u32 = caps["number"].parse().ok()?;
    let event_title = format!("{league} {number}");
    let title = event_title.clone();
    let parsed = SportParsed {
        league_name: league,
        season: None,
        round: Some(number),
        location: None,
        session_type: None,
        event_title: Some(event_title),
        is_pack: false,
        year: None,
    };
    Some((title, ParsedMedia::Sport(parsed), 0.85))
}

fn try_teams(name: &str) -> Option<(String, ParsedMedia, f64)> {
    let caps = TEAMS_RE.captures(name)?;
    let league = clean_league(&caps["league"]);
    let year: u32 = caps["year"].parse().ok()?;
    let team1 = caps["team1"].replace('.', " ").trim().to_string();
    let team2 = caps["team2"].replace('.', " ").trim().to_string();

    let event_title = format!("{team1} vs {team2}");
    let title = format!("{league} {year} {event_title}");
    let parsed = SportParsed {
        league_name: league,
        season: Some(year),
        round: None,
        location: None,
        session_type: None,
        event_title: Some(event_title),
        is_pack: false,
        year: Some(year),
    };

    Some((title, ParsedMedia::Sport(parsed), 0.80))
}

fn try_season_pack(name: &str) -> Option<(String, ParsedMedia, f64)> {
    let caps = SEASON_PACK_RE.captures(name)?;
    let league = clean_league(&caps["league"]);
    let year: u32 = caps["year"].parse().ok()?;

    let title = format!("{league} {year}");
    let parsed = SportParsed {
        league_name: league,
        season: Some(year),
        round: None,
        location: None,
        session_type: None,
        event_title: None,
        is_pack: true,
        year: Some(year),
    };

    Some((title, ParsedMedia::Sport(parsed), 0.70))
}

fn parse_fallback(name: &str) -> (String, ParsedMedia, f64) {
    let title = clean_title(name);
    let year = extract_year(name).map(|(y, _)| y);

    // Try to extract league name (everything before the year)
    let league_name = if let Some((_, pos)) = extract_year(name) {
        let raw = &name[..pos];
        let cleaned = raw.replace('.', " ").trim().to_string();
        if cleaned.is_empty() {
            title.clone()
        } else {
            cleaned
        }
    } else {
        title.clone()
    };

    let parsed = SportParsed {
        league_name,
        season: year,
        round: None,
        location: None,
        session_type: None,
        event_title: None,
        is_pack: false,
        year,
    };

    (title, ParsedMedia::Sport(parsed), 0.40)
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_sport(
        name: &str,
        expected_league: &str,
        expected_season: Option<u32>,
        expected_round: Option<u32>,
        expected_location: Option<&str>,
        expected_session: Option<&str>,
        min_confidence: f64,
    ) {
        let (_, media, confidence) = parse_sport(name);
        let sport = media.as_sport().unwrap_or_else(|| {
            panic!("Expected Sport variant for '{name}', got {media:?}");
        });
        assert_eq!(
            sport.league_name, expected_league,
            "League mismatch for '{name}'"
        );
        assert_eq!(
            sport.season, expected_season,
            "Season mismatch for '{name}'"
        );
        assert_eq!(sport.round, expected_round, "Round mismatch for '{name}'");
        assert_eq!(
            sport.location.as_deref(),
            expected_location,
            "Location mismatch for '{name}'"
        );
        assert_eq!(
            sport.session_type.as_deref(),
            expected_session,
            "Session mismatch for '{name}'"
        );
        assert!(
            confidence >= min_confidence,
            "Confidence {confidence} < {min_confidence} for '{name}'"
        );
    }

    // =========================================================================
    // MotoGP
    // =========================================================================

    #[test]
    fn test_motogp_round_race() {
        assert_sport(
            "MotoGP.2024.R01.Thailand.Race.1080p.WEB-DL",
            "MotoGP",
            Some(2024),
            Some(1),
            Some("Thailand"),
            Some("Race"),
            0.95,
        );
    }

    #[test]
    fn test_motogp_round_qualifying() {
        assert_sport(
            "MotoGP.2024.R03.Argentina.Qualifying.720p.HDTV",
            "MotoGP",
            Some(2024),
            Some(3),
            Some("Argentina"),
            Some("Qualifying"),
            0.95,
        );
    }

    #[test]
    fn test_motogp_round_sprint() {
        assert_sport(
            "MotoGP.2024.R05.France.Sprint.1080p.WEB-DL",
            "MotoGP",
            Some(2024),
            Some(5),
            Some("France"),
            Some("Sprint Race"),
            0.95,
        );
    }

    #[test]
    fn test_motogp_fp1() {
        assert_sport(
            "MotoGP.2024.R02.Portugal.FP1.1080p.WEB-DL",
            "MotoGP",
            Some(2024),
            Some(2),
            Some("Portugal"),
            Some("Free Practice 1"),
            0.95,
        );
    }

    #[test]
    fn test_motogp_fp3() {
        assert_sport(
            "MotoGP.2024.R10.Austria.FP3.720p.HDTV",
            "MotoGP",
            Some(2024),
            Some(10),
            Some("Austria"),
            Some("Free Practice 3"),
            0.95,
        );
    }

    #[test]
    fn test_motogp_practice() {
        assert_sport(
            "MotoGP.2024.R07.Italy.Practice.1080p.WEB-DL",
            "MotoGP",
            Some(2024),
            Some(7),
            Some("Italy"),
            Some("Practice"),
            0.95,
        );
    }

    // =========================================================================
    // Formula 1
    // =========================================================================

    #[test]
    fn test_f1_round_location() {
        assert_sport(
            "Formula.1.2024.R05.Monaco.1080p.WEB-DL",
            "Formula 1",
            Some(2024),
            Some(5),
            Some("Monaco"),
            None,
            0.90,
        );
    }

    #[test]
    fn test_f1_qualifying() {
        assert_sport(
            "Formula.1.2024.R10.Hungary.Qualifying.1080p.WEB-DL",
            "Formula 1",
            Some(2024),
            Some(10),
            Some("Hungary"),
            Some("Qualifying"),
            0.95,
        );
    }

    #[test]
    fn test_f1_free_practice() {
        assert_sport(
            "Formula.1.2025.R01.Australia.FP2.1080p.WEB-DL",
            "Formula 1",
            Some(2025),
            Some(1),
            Some("Australia"),
            Some("Free Practice 2"),
            0.95,
        );
    }

    // =========================================================================
    // Year+Location (no round)
    // =========================================================================

    #[test]
    fn test_year_location_session() {
        assert_sport(
            "MotoGP.2024.Thailand.Race.1080p",
            "MotoGP",
            Some(2024),
            None,
            Some("Thailand"),
            Some("Race"),
            0.85,
        );
    }

    #[test]
    fn test_year_location_only() {
        assert_sport(
            "MotoGP.2024.Thailand.1080p",
            "MotoGP",
            Some(2024),
            None,
            Some("Thailand"),
            None,
            0.75,
        );
    }

    // =========================================================================
    // YEARxROUND format
    // =========================================================================

    #[test]
    fn test_year_x_round_with_location_and_session() {
        assert_sport(
            "MotoGP.2024x02.Portugal.Race.1080p.WEB-DL",
            "MotoGP",
            Some(2024),
            Some(2),
            Some("Portugal"),
            Some("Race"),
            0.90,
        );
    }

    #[test]
    fn test_year_x_round_location_only() {
        assert_sport(
            "MotoGP.2024x05.France.1080p.WEB-DL",
            "MotoGP",
            Some(2024),
            Some(5),
            Some("France"),
            None,
            0.90,
        );
    }

    // =========================================================================
    // UFC / Event number
    // =========================================================================

    #[test]
    fn test_ufc_numbered() {
        let (_, media, confidence) = parse_sport("UFC.302.Main.Card.1080p.WEB-DL");
        let sport = media.as_sport().unwrap();
        assert_eq!(sport.league_name, "UFC");
        assert_eq!(sport.round, Some(302));
        assert!(confidence >= 0.85);
    }

    #[test]
    fn test_ufc_no_session() {
        // UFC release with no session info (just number + quality)
        let (_, media, confidence) = parse_sport("UFC.302.1080p.WEB.h264-GROUP");
        let sport = media.as_sport().unwrap();
        assert_eq!(sport.league_name, "UFC");
        assert_eq!(sport.round, Some(302));
        assert!(
            sport.session_type.is_none(),
            "Generic UFC release should have no session_type, got: {:?}",
            sport.session_type
        );
        assert!(confidence >= 0.85);
    }

    #[test]
    fn test_ufc_early_prelims() {
        let (_, media, confidence) = parse_sport("UFC.298.Early.Prelims.720p.HDTV");
        let sport = media.as_sport().unwrap();
        assert_eq!(sport.league_name, "UFC");
        assert_eq!(sport.round, Some(298));
        assert!(confidence >= 0.85);
    }

    #[test]
    fn test_bellator() {
        let (_, media, confidence) = parse_sport("Bellator.301.Main.Card.1080p.WEB-DL");
        let sport = media.as_sport().unwrap();
        assert_eq!(sport.league_name, "Bellator");
        assert_eq!(sport.round, Some(301));
        assert!(confidence >= 0.85);
    }

    // =========================================================================
    // Teams format (football/soccer)
    // =========================================================================

    #[test]
    fn test_teams_vs() {
        let (_, media, confidence) =
            parse_sport("Premier.League.2024.Arsenal.vs.Chelsea.1080p.WEB-DL");
        let sport = media.as_sport().unwrap();
        assert_eq!(sport.league_name, "Premier League");
        assert_eq!(sport.season, Some(2024));
        assert_eq!(sport.event_title.as_deref(), Some("Arsenal vs Chelsea"));
        assert!(confidence >= 0.80);
    }

    #[test]
    fn test_teams_v() {
        let (_, media, confidence) = parse_sport("La.Liga.2024.Barcelona.v.Real.Madrid.1080p.HDTV");
        let sport = media.as_sport().unwrap();
        assert_eq!(sport.league_name, "La Liga");
        assert_eq!(sport.season, Some(2024));
        assert!(sport.event_title.is_some());
        assert!(confidence >= 0.80);
    }

    // =========================================================================
    // Season packs
    // =========================================================================

    #[test]
    fn test_season_pack_complete() {
        let (_, media, confidence) = parse_sport("MotoGP.2024.Season.Complete.1080p.WEB-DL");
        let sport = media.as_sport().unwrap();
        assert_eq!(sport.league_name, "MotoGP");
        assert_eq!(sport.season, Some(2024));
        assert!(sport.is_pack);
        assert!(confidence >= 0.70);
    }

    #[test]
    fn test_season_pack_full() {
        let (_, media, confidence) = parse_sport("Formula.1.2023.Full.Season.1080p.WEB-DL");
        let sport = media.as_sport().unwrap();
        assert_eq!(sport.league_name, "Formula 1");
        assert_eq!(sport.season, Some(2023));
        assert!(sport.is_pack);
        assert!(confidence >= 0.70);
    }

    // =========================================================================
    // Fallback
    // =========================================================================

    #[test]
    fn test_fallback_with_year() {
        // Title that doesn't fit any known shape: league + year, then a
        // numeric-only trailing token that can't be a location.
        let (_, media, confidence) = parse_sport("SomeUnknownSport.2024.0815");
        let sport = media.as_sport().unwrap();
        assert_eq!(sport.season, Some(2024));
        assert!(confidence >= 0.40);
        assert!(confidence < 0.70);
    }

    #[test]
    fn test_fallback_no_year() {
        let (_, media, confidence) = parse_sport("RandomSportEvent");
        let sport = media.as_sport().unwrap();
        assert!(confidence < 0.50);
        assert!(sport.season.is_none());
    }

    #[test]
    fn test_sprint_race_not_sprint() {
        // "Sprint Race" must not be truncated to "Sprint"
        assert_sport(
            "Formula.1.2024.R01.Thailand.Sprint.Race.1080p.WEB-DL",
            "Formula 1",
            Some(2024),
            Some(1),
            Some("Thailand"),
            Some("Sprint Race"),
            0.95,
        );
    }

    #[test]
    fn test_sprint_race_year_loc_session() {
        // Year+Location+Session variant (no round → 0.85 confidence)
        assert_sport(
            "Formula.1.2024.Thailand.Sprint.Race.1080p",
            "Formula 1",
            Some(2024),
            None,
            Some("Thailand"),
            Some("Sprint Race"),
            0.85,
        );
    }

    #[test]
    fn test_sprint_normalizes_to_sprint_race() {
        // Bare "Sprint" normalizes to "Sprint Race" — they're the same session
        assert_sport(
            "Formula.1.2024.R01.Thailand.Sprint.1080p.WEB-DL",
            "Formula 1",
            Some(2024),
            Some(1),
            Some("Thailand"),
            Some("Sprint Race"),
            0.95,
        );
    }

    #[test]
    fn test_race_stays_race() {
        // Plain "Race" must still parse as "Race"
        assert_sport(
            "Formula.1.2024.R01.Thailand.Race.1080p.WEB-DL",
            "Formula 1",
            Some(2024),
            Some(1),
            Some("Thailand"),
            Some("Race"),
            0.95,
        );
    }

    // =========================================================================
    // Simple hyphen-separated: "League - R{round} - Event Title"
    // =========================================================================

    #[test]
    fn test_simple_round_event_with_session() {
        // "Moto2 - R01 - Thailand Grand Prix Race.mkv" → event_title + session
        let (_, media, confidence) = parse_sport("Moto2 - R01 - Thailand Grand Prix Race.mkv");
        let sport = media.as_sport().unwrap();
        assert_eq!(sport.league_name, "Moto2");
        assert_eq!(sport.round, Some(1));
        assert_eq!(sport.event_title.as_deref(), Some("Thailand Grand Prix"));
        assert_eq!(sport.session_type.as_deref(), Some("Race"));
        assert!(sport.season.is_none()); // year comes from folder
        assert!(confidence >= 0.90);
    }

    #[test]
    fn test_simple_round_event_no_session() {
        // "Moto2 - R01 - Thailand Grand Prix Moto2.mkv" → event_title only (no session suffix)
        let (_, media, confidence) = parse_sport("Moto2 - R01 - Thailand Grand Prix Moto2.mkv");
        let sport = media.as_sport().unwrap();
        assert_eq!(sport.league_name, "Moto2");
        assert_eq!(sport.round, Some(1));
        assert_eq!(
            sport.event_title.as_deref(),
            Some("Thailand Grand Prix Moto2")
        );
        assert!(sport.session_type.is_none());
        assert!(confidence >= 0.90);
    }

    #[test]
    fn test_simple_round_event_qualifying() {
        let (_, media, confidence) =
            parse_sport("MotoGP - R02 - Portugal Grand Prix Qualifying.mkv");
        let sport = media.as_sport().unwrap();
        assert_eq!(sport.league_name, "MotoGP");
        assert_eq!(sport.round, Some(2));
        assert_eq!(sport.event_title.as_deref(), Some("Portugal Grand Prix"));
        assert_eq!(sport.session_type.as_deref(), Some("Qualifying"));
        assert!(confidence >= 0.90);
    }

    #[test]
    fn test_simple_round_event_sprint_race() {
        let (_, media, confidence) = parse_sport("MotoGP - R05 - France Sprint Race.mkv");
        let sport = media.as_sport().unwrap();
        assert_eq!(sport.league_name, "MotoGP");
        assert_eq!(sport.round, Some(5));
        assert_eq!(sport.event_title.as_deref(), Some("France"));
        assert_eq!(sport.session_type.as_deref(), Some("Sprint Race"));
        assert!(confidence >= 0.90);
    }

    #[test]
    fn test_simple_round_event_formula1() {
        let (_, media, confidence) = parse_sport("Formula 1 - R10 - British Grand Prix Race.mkv");
        let sport = media.as_sport().unwrap();
        assert_eq!(sport.league_name, "Formula 1");
        assert_eq!(sport.round, Some(10));
        assert_eq!(sport.event_title.as_deref(), Some("British Grand Prix"));
        assert_eq!(sport.session_type.as_deref(), Some("Race"));
        assert!(confidence >= 0.90);
    }

    // =========================================================================
    // Word-number session parsing (Qualifying One/Two, etc.)
    // =========================================================================

    #[test]
    fn test_qualifying_two_round_loc() {
        // "Qualifying.Two" must parse as "Qualifying 2"
        assert_sport(
            "MotoGP.2026.Round03.USA.Qualifying.Two.720p.WEB.H.264-JFF",
            "MotoGP",
            Some(2026),
            Some(3),
            Some("USA"),
            Some("Qualifying 2"),
            0.95,
        );
    }

    #[test]
    fn test_qualifying_one_round_loc() {
        assert_sport(
            "MotoGP.2026.R03.USA.Qualifying.One.1080p.WEB.h264-BILLIE",
            "MotoGP",
            Some(2026),
            Some(3),
            Some("USA"),
            Some("Qualifying 1"),
            0.95,
        );
    }

    #[test]
    fn test_qualifying_two_year_loc() {
        // Year+Location variant (no round)
        assert_sport(
            "MotoGP.2026.USA.Qualifying.Two.1080p.WEB.h264-BILLIE",
            "MotoGP",
            Some(2026),
            None,
            Some("USA"),
            Some("Qualifying 2"),
            0.85,
        );
    }

    #[test]
    fn test_bare_sprint_normalizes_to_sprint_race() {
        // Bare "Sprint" in release must normalize to "Sprint Race"
        // so it matches wants with session_type="Sprint Race"
        assert_sport(
            "MotoGP.2026.Round03.USA.Sprint.WEB-DL.1080p.H264.English-MWR",
            "MotoGP",
            Some(2026),
            Some(3),
            Some("USA"),
            Some("Sprint Race"),
            0.95,
        );
    }

    #[test]
    fn test_free_practice_one_round_loc() {
        assert_sport(
            "MotoGP.2026.R03.USA.Free.Practice.One.1080p.WEB-GROUP",
            "MotoGP",
            Some(2026),
            Some(3),
            Some("USA"),
            Some("Free Practice 1"),
            0.95,
        );
    }

    // ── SBK (Superbike) ─────────────────────────────────────────────────

    #[test]
    fn test_sbk_race_two_round_loc() {
        assert_sport(
            "SBK.2026.R02.Portugal.Race.Two.1080p.WEB-DL",
            "SBK",
            Some(2026),
            Some(2),
            Some("Portugal"),
            Some("Race 2"),
            0.95,
        );
    }

    #[test]
    fn test_sbk_race_one_round_loc() {
        assert_sport(
            "SBK.2026.R02.Portugal.Race.One.1080p.WEB-DL",
            "SBK",
            Some(2026),
            Some(2),
            Some("Portugal"),
            Some("Race 1"),
            0.95,
        );
    }

    #[test]
    fn test_sbk_race_2_digit_round_loc() {
        assert_sport(
            "SBK.2026.R02.Portugal.Race.2.1080p.WEB-DL",
            "SBK",
            Some(2026),
            Some(2),
            Some("Portugal"),
            Some("Race 2"),
            0.95,
        );
    }

    #[test]
    fn test_sbk_superpole_race_round_loc() {
        assert_sport(
            "SBK.2026.R02.Portugal.Superpole.Race.1080p.WEB-DL",
            "SBK",
            Some(2026),
            Some(2),
            Some("Portugal"),
            Some("Superpole Race"),
            0.95,
        );
    }

    #[test]
    fn test_sbk_superpole_round_loc() {
        assert_sport(
            "SBK.2026.R02.Portugal.Superpole.1080p.WEB-DL",
            "SBK",
            Some(2026),
            Some(2),
            Some("Portugal"),
            Some("Superpole"),
            0.95,
        );
    }

    #[test]
    fn test_sbk_race_two_year_loc() {
        // No round variant
        assert_sport(
            "SBK.2026.Portugal.Race.Two.1080p.WEB-DL",
            "SBK",
            Some(2026),
            None,
            Some("Portugal"),
            Some("Race 2"),
            0.85,
        );
    }

    // =========================================================================
    // Underscore-separated release names (NZB / scene convention)
    // =========================================================================

    #[test]
    fn test_underscore_year_loc_session_practice() {
        // Real example: SABnzbd / DrunkenSlug release names use underscores.
        // Without underscore-as-separator support, parse_sport falls through
        // to parse_fallback and session_type stays None — which means a
        // SportEvent want for the Race silently accepts the Practice release.
        assert_sport(
            "Moto2_2026_France_Practice_1080p_WEB_h264-VERUM",
            "Moto2",
            Some(2026),
            None,
            Some("France"),
            Some("Practice"),
            0.85,
        );
    }

    #[test]
    fn test_underscore_year_loc_session_qualifying() {
        assert_sport(
            "Moto3_2026_France_Qualifying_1080p_WEB_h264-VERUM",
            "Moto3",
            Some(2026),
            None,
            Some("France"),
            Some("Qualifying"),
            0.85,
        );
    }

    #[test]
    fn test_underscore_round_loc_session_race() {
        assert_sport(
            "MotoGP_2026_R05_France_Race_1080p_WEB-DL",
            "MotoGP",
            Some(2026),
            Some(5),
            Some("France"),
            Some("Race"),
            0.95,
        );
    }

    #[test]
    fn test_underscore_prod_verum_free_practice_one() {
        // Exact prod title from the 2026-05-08 incident: an FP1 clip was
        // grabbed for a Race want because the underscore separators stopped
        // parse_sport from recognising the session. After normalization the
        // session must resolve to "Free Practice 1".
        assert_sport(
            "Moto3_2026_Hungary_Free_Practice_One_1080p_WEB_h264-VERUM",
            "Moto3",
            Some(2026),
            None,
            Some("Hungary"),
            Some("Free Practice 1"),
            0.85,
        );
    }
}
