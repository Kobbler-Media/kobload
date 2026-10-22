//! What Kobbler's `sport_want` sends for a sport event, rebuilt from the same
//! inputs it reads: the Sportarr season schedule, the MotoGP results API's
//! events, Sportarr's league alternate names and the GeoNames gazetteer.
//! It mirrors `sport_want::{aliases, enrich, gazetteer}` in Kobbler; keep
//! the two in step, or the evaluation stops measuring what Kobbler sends.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde_json::{Value, json};
use unicode_normalization::UnicodeNormalization;
use unicode_normalization::char::is_combining_mark;

const VENUE_FILLER: &[&str] = &[
    "autodrom",
    "autodromo",
    "automotodrom",
    "circuit",
    "circuito",
    "grand",
    "internacional",
    "international",
    "internazionale",
    "motorland",
    "park",
    "prix",
    "street",
    "world",
];
const PARTICLES: &[&str] = &[
    "and", "da", "de", "del", "della", "des", "di", "do", "du", "e", "la", "le", "of", "the", "von",
];
const VENUE_NICKNAMES: &[(&str, &[&str])] = &[("circuit of the americas", &["COTA"])];
const EXTRA_NAMES: &[(&str, &[&str])] = &[
    ("US", &["United States of America", "America"]),
    ("GB", &["UK", "Great Britain", "Britain"]),
    ("CZ", &["Czech Republic"]),
    ("NL", &["Netherlands", "Holland"]),
    ("AE", &["UAE"]),
    ("KR", &["Korea"]),
];
const DEMONYMS: &[(&str, &[&str])] = &[
    ("AE", &["Emirati"]),
    ("AR", &["Argentine", "Argentinian", "Argentinean"]),
    ("AT", &["Austrian"]),
    ("AU", &["Australian"]),
    ("AZ", &["Azerbaijani"]),
    ("BE", &["Belgian"]),
    ("BH", &["Bahraini"]),
    ("BR", &["Brazilian"]),
    ("CA", &["Canadian"]),
    ("CH", &["Swiss"]),
    ("CN", &["Chinese"]),
    ("CZ", &["Czech"]),
    ("DE", &["German"]),
    ("ES", &["Spanish"]),
    ("FI", &["Finnish"]),
    ("FR", &["French"]),
    ("GB", &["British"]),
    ("HR", &["Croatian"]),
    ("HU", &["Hungarian"]),
    ("ID", &["Indonesian"]),
    ("IN", &["Indian"]),
    ("IT", &["Italian"]),
    ("JP", &["Japanese"]),
    ("KR", &["Korean"]),
    ("KZ", &["Kazakh"]),
    ("MC", &["Monegasque"]),
    ("MX", &["Mexican"]),
    ("MY", &["Malaysian"]),
    ("NL", &["Dutch"]),
    ("PL", &["Polish"]),
    ("PT", &["Portuguese"]),
    ("QA", &["Qatari"]),
    ("SA", &["Saudi"]),
    ("SE", &["Swedish"]),
    ("SG", &["Singaporean"]),
    ("SI", &["Slovenian"]),
    ("SM", &["Sammarinese"]),
    ("TH", &["Thai"]),
    ("TR", &["Turkish"]),
    ("US", &["American"]),
    ("VN", &["Vietnamese"]),
    ("ZA", &["South African"]),
];

fn fold(s: &str) -> String {
    s.nfd()
        .filter(|c| !is_combining_mark(*c))
        .collect::<String>()
        .to_lowercase()
}

struct Country {
    iso2: String,
    iso3: String,
    name: String,
}

pub struct Sources {
    countries: Vec<Country>,
    regions: HashMap<(String, String), String>,
    schedules: HashMap<&'static str, Vec<Value>>,
    motogp_events: Vec<Value>,
    league_names: HashMap<&'static str, Vec<String>>,
}

fn tsv(path: &Path) -> Vec<Vec<String>> {
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("{}: {e}", path.display()))
        .lines()
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| l.split('\t').map(String::from).collect())
        .collect()
}

fn json_file(path: &Path) -> Value {
    serde_json::from_str(
        &std::fs::read_to_string(path).unwrap_or_else(|e| panic!("{}: {e}", path.display())),
    )
    .unwrap()
}

const LEAGUES: &[(&str, &str, &str)] = &[
    ("MotoGP", "motogp", "lg-000046"),
    ("Moto2", "moto2", "lg-000072"),
    ("Moto3", "moto3", "lg-000073"),
    ("SBK", "sbk", "lg-000090"),
];

impl Sources {
    pub fn load(dir: &Path) -> Self {
        let countries = tsv(&dir.join("countries.tsv"))
            .into_iter()
            .filter_map(|r| match &r[..] {
                [iso2, iso3, name] => Some(Country {
                    iso2: iso2.clone(),
                    iso3: iso3.clone(),
                    name: name.clone(),
                }),
                _ => None,
            })
            .collect();
        let regions = tsv(&dir.join("cities.tsv"))
            .into_iter()
            .filter_map(|r| match &r[..] {
                [city, iso2, region] => Some(((fold(city), iso2.clone()), region.clone())),
                _ => None,
            })
            .collect();
        let mut schedules = HashMap::new();
        let mut league_names = HashMap::new();
        for (league, file, id) in LEAGUES {
            let s = json_file(&dir.join(format!("sportarr/{file}.json")));
            schedules.insert(
                *league,
                s["data"]["schedule"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default(),
            );
            let l = json_file(&dir.join(format!("leagues/{id}.json")));
            league_names.insert(
                *league,
                l["alternateNames"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(|v| v.as_str().map(|s| s.trim().to_string()))
                    .filter(|s| !s.is_empty())
                    .collect(),
            );
        }
        Self {
            countries,
            regions,
            schedules,
            motogp_events: json_file(&dir.join("pulselive_events.json"))
                .as_array()
                .cloned()
                .unwrap_or_default(),
            league_names,
        }
    }

    fn country(&self, value: &str) -> Option<&Country> {
        let value = value.trim();
        if value.len() == 2 {
            return self
                .countries
                .iter()
                .find(|c| c.iso2.eq_ignore_ascii_case(value));
        }
        let wanted = fold(value);
        self.countries
            .iter()
            .find(|c| country_aliases(c).iter().any(|a| fold(a) == wanted))
    }
}

fn country_aliases(c: &Country) -> Vec<String> {
    let listed = |table: &[(&str, &[&str])]| -> Vec<String> {
        table
            .iter()
            .filter(|(iso2, _)| *iso2 == c.iso2)
            .flat_map(|(_, names)| names.iter().map(|n| n.to_string()))
            .collect()
    };
    let mut out = vec![c.name.clone()];
    out.extend(listed(EXTRA_NAMES));
    out.push(c.iso2.clone());
    out.push(c.iso3.clone());
    out.extend(listed(DEMONYMS));
    out
}

fn class_of(league_name: &str) -> Option<&'static str> {
    let key: String = league_name
        .chars()
        .filter(|c| c.is_alphanumeric())
        .collect::<String>()
        .to_lowercase();
    Some(match key.as_str() {
        "motogp" => "MotoGP",
        "moto2" => "Moto2",
        "moto3" => "Moto3",
        "motoe" => "MotoE",
        "sbk"
        | "wsbk"
        | "worldsbk"
        | "worldsuperbike"
        | "worldsuperbikes"
        | "superbikeworldchampionship" => "SBK",
        "wssp" | "worldssp" | "worldsupersport" | "supersportworldchampionship" => "WorldSSP",
        "f1" | "formula1" | "formulaone" => "F1",
        "f2" | "formula2" => "F2",
        "f3" | "formula3" => "F3",
        "formulae" => "Formula E",
        _ => return None,
    })
}

fn tidy(name: &str) -> Option<String> {
    let collapsed = name.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return None;
    }
    let shouting = collapsed.chars().filter(|c| c.is_alphabetic()).count() > 3
        && !collapsed.chars().any(char::is_lowercase);
    if !shouting {
        return Some(collapsed);
    }
    Some(
        collapsed
            .split(' ')
            .map(|word| {
                let lower = word.to_lowercase();
                let mut chars = lower.chars();
                chars
                    .next()
                    .map(|first| first.to_uppercase().chain(chars).collect())
                    .unwrap_or_default()
            })
            .collect::<Vec<String>>()
            .join(" "),
    )
}

fn chunks(name: &str, splitters: &[&str]) -> Vec<String> {
    let mut out = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    let mut flush = |current: &mut Vec<&str>| {
        let chunk = current.join(" ");
        if chunk.chars().count() >= 3 && !PARTICLES.contains(&fold(&chunk).as_str()) {
            out.push(chunk);
        }
        current.clear();
    };
    for piece in name.split(['-', '–', '—', ',', '/', '(', ')']) {
        for word in piece.split_whitespace() {
            let folded = fold(word);
            if splitters.contains(&folded.as_str()) || PARTICLES.contains(&folded.as_str()) {
                flush(&mut current);
            } else {
                current.push(word);
            }
        }
        flush(&mut current);
    }
    out
}

fn venue_names(venue: &str) -> Vec<String> {
    let mut names = vec![venue.to_string()];
    let folded = fold(venue);
    for (full, nicknames) in VENUE_NICKNAMES {
        if folded == *full {
            names.extend(nicknames.iter().map(|n| n.to_string()));
        }
    }
    names.extend(
        chunks(venue, VENUE_FILLER)
            .into_iter()
            .filter(|c| c != venue),
    );
    names
}

fn city_names(city: &str) -> Vec<String> {
    city.split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .flat_map(|part| {
            std::iter::once(part.to_string())
                .chain(chunks(part, &[]).into_iter().filter(move |c| c != part))
        })
        .collect()
}

#[derive(Default)]
struct AliasList {
    seen: HashSet<String>,
    names: Vec<String>,
}

impl AliasList {
    fn push(&mut self, name: &str) {
        let name = name.trim();
        if !name.is_empty() && self.seen.insert(fold(name)) {
            self.names.push(name.to_string());
        }
    }
}

fn round_of(e: &Value) -> Option<i64> {
    e["intRound"]
        .as_i64()
        .or_else(|| e["intRound"].as_str().and_then(|r| r.parse().ok()))
}

fn str_of<'v>(v: &'v Value, key: &str) -> Option<&'v str> {
    v[key].as_str().map(str::trim).filter(|s| !s.is_empty())
}

fn session_start(timestamp: Option<&str>) -> Option<String> {
    let t = time::OffsetDateTime::parse(timestamp?, &time::format_description::well_known::Rfc3339)
        .ok()?;
    (t.to_offset(time::UtcOffset::UTC).time() != time::Time::MIDNIGHT)
        .then(|| timestamp.unwrap().to_string())
}

pub fn meta_for(sources: &Sources, want: &Value) -> Option<Value> {
    let league = want["league_name"].as_str()?;
    let date = want["event_date"].as_str()?;
    let title = want["event_title"]
        .as_str()
        .map(|t| t.split_whitespace().collect::<Vec<_>>().join(" "));
    let location = want["location"].as_str();
    let class = class_of(league);
    let schedule = sources.schedules.get(league)?;

    let mut venues = Vec::new();
    let mut cities = Vec::new();
    let mut places = Vec::new();
    let mut country_iso: Option<String> = None;
    let country_from = |value: Option<&str>, iso: &mut Option<String>| {
        if iso.is_some() {
            return;
        }
        if let Some(c) = value.and_then(|v| sources.country(v)) {
            *iso = Some(c.iso2.clone());
        }
    };
    let mut event_aliases = Vec::new();
    let mut start = None;

    let same_day: Vec<&Value> = schedule
        .iter()
        .filter(|e| e["dateEvent"].as_str() == Some(date))
        .collect();
    let found = same_day
        .iter()
        .find(|e| {
            str_of(e, "strEvent").map(|s| s.split_whitespace().collect::<Vec<_>>().join(" "))
                == title
        })
        .or_else(|| same_day.first())
        .copied();
    let round = found
        .and_then(round_of)
        .or_else(|| want["round"].as_i64())
        .filter(|r| *r > 0);
    let mut rows: Vec<&Value> = found.into_iter().collect();
    if let Some(round) = round {
        rows.extend(
            schedule.iter().filter(|e| {
                round_of(e) == Some(round) && found.is_none_or(|f| !std::ptr::eq(*e, f))
            }),
        );
    }
    for row in &rows {
        venues.extend(str_of(row, "strVenue").map(String::from));
        cities.extend(str_of(row, "strCity").map(String::from));
        country_from(str_of(row, "strCountry"), &mut country_iso);
    }
    if let Some(f) = found {
        start = session_start(str_of(f, "strTimestamp"));
        event_aliases.extend(str_of(f, "strEventAlternate").map(String::from));
        event_aliases.extend(str_of(f, "strFilename").map(String::from));
    }

    if matches!(class, Some("MotoGP" | "Moto2" | "Moto3"))
        && let Some(ev) = sources.motogp_events.iter().find(|e| {
            !e["test"].as_bool().unwrap_or(false)
                && e["date_start"].as_str().is_some_and(|s| s <= date)
                && e["date_end"].as_str().is_some_and(|s| date <= s)
        })
    {
        venues.extend(ev["circuit"]["name"].as_str().and_then(tidy));
        cities.extend(ev["circuit"]["place"].as_str().and_then(tidy));
        places.extend(ev["additional_name"].as_str().and_then(tidy));
        country_from(ev["country"]["iso"].as_str(), &mut country_iso);
        places.extend(ev["country"]["name"].as_str().and_then(tidy));
        event_aliases.extend(
            [&ev["sponsored_name"], &ev["name"]]
                .into_iter()
                .filter_map(|n| n.as_str().and_then(tidy)),
        );
    }
    country_from(location, &mut country_iso);

    let country = country_iso.as_deref().and_then(|iso| sources.country(iso));
    let mut location_aliases = AliasList::default();
    if let Some(location) = location {
        location_aliases.push(location);
    }
    for venue in &venues {
        for name in venue_names(venue) {
            location_aliases.push(&name);
        }
    }
    let mut city_parts = Vec::new();
    for city in &cities {
        for name in city_names(city) {
            location_aliases.push(&name);
            city_parts.push(name);
        }
    }
    for place in &places {
        location_aliases.push(place);
    }
    let region = country.and_then(|c| {
        city_parts
            .iter()
            .find_map(|p| sources.regions.get(&(fold(p), c.iso2.clone())).cloned())
    });
    if let Some(region) = &region {
        location_aliases.push(region);
    }
    if let Some(c) = country {
        for name in country_aliases(c) {
            location_aliases.push(&name);
        }
    }
    let mut events = AliasList::default();
    for name in &event_aliases {
        events.push(name);
    }
    let mut leagues = AliasList::default();
    leagues.seen.insert(fold(league.trim()));
    for name in sources.league_names.get(league).into_iter().flatten() {
        leagues.push(name);
    }

    let some = |v: Vec<String>| (!v.is_empty()).then_some(v);
    Some(json!({
        "venue": venues.first(),
        "city": cities.first().and_then(|c| c.split(',').next()).map(|c| c.trim().to_string()),
        "region": region,
        "country": country.map(|c| c.name.clone()),
        "country_iso": country.map(|c| c.iso2.clone()),
        "location_aliases": some(location_aliases.names),
        "event_aliases": some(events.names),
        "league_aliases": some(leagues.names),
        "session_start": start,
        "class": class,
    }))
}
