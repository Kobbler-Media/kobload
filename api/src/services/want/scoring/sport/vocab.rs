use std::sync::LazyLock;

use super::tokens::normalize_phrase;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot {
    Class,
    Session,
    Filler,
    Broadcaster,
    Event,
}

#[derive(Debug, Clone)]
pub struct Phrase {
    pub words: Vec<String>,
    pub slot: Slot,
    pub value: String,
}

fn phrases(entries: &[(&str, &[&str])], slot: Slot) -> Vec<Phrase> {
    entries
        .iter()
        .flat_map(|(value, forms)| {
            forms.iter().map(move |f| Phrase {
                words: normalize_phrase(f),
                slot,
                value: (*value).to_string(),
            })
        })
        .filter(|p| !p.words.is_empty())
        .collect()
}

pub(super) const CLASSES: &[(&str, &[&str])] = &[
    ("motogp", &["MotoGP", "Moto GP"]),
    ("moto2", &["Moto2", "Moto 2"]),
    ("moto3", &["Moto3", "Moto 3"]),
    ("motoe", &["MotoE", "Moto E"]),
    (
        "sbk",
        &[
            "SBK",
            "WSBK",
            "WorldSBK",
            "World Superbike",
            "World Superbikes",
            "Superbike World Championship",
        ],
    ),
    (
        "wssp",
        &["WSSP", "WorldSSP", "World Supersport", "Supersport"],
    ),
    (
        "wssp300",
        &["WSSP300", "SSP300", "Supersport 300", "WorldSSP300"],
    ),
    ("wcr", &["WCR", "WorldWCR"]),
    ("bsb", &["British Superbikes", "British Superbike", "BSB"]),
    (
        "f1",
        &[
            "F1",
            "Formula 1",
            "Formula One",
            "Formula1",
            "Formula Uno",
            "Formula 1 Grand Prix",
        ],
    ),
    ("f1academy", &["F1 Academy"]),
    ("f2", &["F2", "Formula 2"]),
    ("f3", &["F3", "Formula 3"]),
    ("indycar", &["IndyCar", "Indy Car"]),
    ("indynxt", &["Indy NXT", "IndyNXT"]),
    (
        "supercup",
        &["Porsche Supercup", "Porsche Mobil 1 Supercup", "Supercup"],
    ),
    ("formulae", &["Formula E"]),
    ("nascar", &["NASCAR"]),
    ("wec", &["WEC"]),
    ("dtm", &["DTM"]),
    ("btcc", &["BTCC"]),
    ("wrc", &["WRC"]),
    ("mxgp", &["MXGP", "Motocross"]),
];

pub(super) const UMBRELLA: &[(&str, &str)] = &[
    ("moto2", "motogp"),
    ("moto3", "motogp"),
    ("motoe", "motogp"),
    ("wssp", "sbk"),
    ("wssp300", "sbk"),
    ("wcr", "sbk"),
    ("f1academy", "f1"),
];

const SESSIONS: &[(&str, &[&str])] = &[
    (
        "race",
        &[
            "Race",
            "Full Race",
            "Main Race",
            "GP Race",
            "Full Race Session",
            "Carrera",
            "Gara",
            "Course",
            "La Course",
            "Futam",
            "Zavod",
            "Rennen",
            "Wyscig",
            "Corrida",
        ],
    ),
    ("race1", &["Race 1", "Race One", "Carrera 1", "Gara 1"]),
    ("race2", &["Race 2", "Race Two", "Carrera 2", "Gara 2"]),
    ("superpole_race", &["Superpole Race", "Superpole Carrera"]),
    ("superpole", &["Superpole"]),
    (
        "sprint",
        &[
            "Sprint",
            "Sprint Race",
            "SprintRace",
            "Sprintfutam",
            "Course Sprint",
            "La Course Sprint",
            "Carrera Sprint",
            "Tissot Sprint",
        ],
    ),
    (
        "sprint_q",
        &[
            "Sprint Qualifying",
            "Sprint Shootout",
            "Sprint Pole",
            "Sprint Quali",
            "Sprint Qualy",
            "Sprint Qualification",
        ],
    ),
    ("practice", &["Practice", "Practica"]),
    (
        "fp",
        &[
            "Free Practice",
            "Libres",
            "Szabadedzes",
            "Entrenamientos Libres",
        ],
    ),
    (
        "fp1",
        &[
            "FP1",
            "Free Practice 1",
            "Libres 1",
            "1 Szabadedzes",
            "Practice One",
        ],
    ),
    (
        "fp2",
        &[
            "FP2",
            "Free Practice 2",
            "Libres 2",
            "2 Szabadedzes",
            "Practice Two",
        ],
    ),
    (
        "fp3",
        &["FP3", "Free Practice 3", "Libres 3", "3 Szabadedzes"],
    ),
    ("fp4", &["FP4", "Free Practice 4", "Libres 4"]),
    ("q1", &["Q1", "Qualifying 1", "Qualifying One"]),
    ("q2", &["Q2", "Qualifying 2", "Qualifying Two"]),
    (
        "qualifying",
        &[
            "Qualifying",
            "Qualy",
            "Quali",
            "Qual",
            "Qualification",
            "Clasificacion",
            "Idomero",
            "Pole",
            "Kvalifikace",
            "Qualifiche",
            "Qualifications",
        ],
    ),
    ("warmup", &["Warm Up", "Warmup"]),
    (
        "press",
        &[
            "Press Conference",
            "Pre Event Press Conference",
            "Post Event Press Conference",
            "Pre-Event Press Conference",
        ],
    ),
    (
        "test",
        &[
            "Test",
            "Testing",
            "Tests",
            "Official Test",
            "Test Day",
            "Shakedown",
            "Test Upload",
        ],
    ),
    ("after_the_flag", &["After The Flag"]),
    ("gear_up", &["Gear Up"]),
    (
        "show",
        &[
            "Pre Race Show",
            "Post Race Show",
            "Pre Qualifying Show",
            "Post Qualifying Show",
            "Pre Sprint Show",
            "Post Sprint Show",
            "Build Up",
            "Grid",
            "Boxutca",
            "Startline",
            "Paddock Pass",
            "Media Day",
        ],
    ),
    (
        "preview",
        &[
            "Season Preview",
            "Season Launch",
            "Season Review",
            "Preview",
            "Review",
            "Launch",
        ],
    ),
    (
        "highlights",
        &["Highlights", "Extended Highlights", "Resumen"],
    ),
    (
        "weekend",
        &[
            "Full Weekend",
            "Weekend",
            "Friday",
            "Races Only",
            "All Sessions",
        ],
    ),
    ("onboard", &["Onboard", "On Board"]),
];

const SPORT_FILLER: &[(&str, &[&str])] = &[(
    "filler",
    &[
        "Grand Prix",
        "GP",
        "Gran Premio",
        "Grande Premio",
        "Grand Prix of",
        "of",
        "the",
        "de",
        "du",
        "di",
        "von",
        "la",
        "le",
        "el",
        "del",
        "round",
        "rd",
        "official",
        "world",
        "championship",
        "motorcycle",
        "live",
        "replay",
        "session",
        "full",
        "and",
        "at",
        "en",
        "english",
        "spa",
        "lat",
        "hun",
        "vff",
        "multi",
        "multiaudio",
        "multi audio",
        "multi5",
        "multilang",
        "multi language",
        "castellano",
        "ita",
        "eng",
        "dornarip",
        "dorna rip",
    ],
)];

const BROADCASTERS: &[(&str, &[&str])] = &[(
    "broadcaster",
    &[
        "TNT",
        "TNT Sports",
        "DAZN",
        "DAZN1",
        "DAZNF1",
        "Sky",
        "Sky Sports",
        "Sky Sports F1",
        "SkyF1",
        "SkyF1HD",
        "SkyF1UHD",
        "SkyUHD",
        "Sky F1",
        "Sky Sports F1 UHD",
        "SkySportsF1UHD",
        "SkySportsItalia",
        "F1TV",
        "F1TV Live",
        "F1TVLive",
        "Polsat",
        "NOW",
        "ESPN",
        "ESPNF1",
        "Eurosport",
        "Canal",
        "Movistar",
        "Servus TV",
        "ServusTV",
        "BT Sport",
        "ITV",
        "Viaplay",
        "RTL",
        "ORF",
        "Rai",
        "TV8",
        "VideoPass",
        "F1GP",
        "F1UHD",
    ],
)];

pub fn closed_vocab() -> &'static [Phrase] {
    static V: LazyLock<Vec<Phrase>> = LazyLock::new(|| {
        let mut v = phrases(CLASSES, Slot::Class);
        v.extend(phrases(SESSIONS, Slot::Session));
        v.extend(phrases(SPORT_FILLER, Slot::Filler));
        v.extend(phrases(BROADCASTERS, Slot::Broadcaster));
        v
    });
    &V
}

fn lookup(entries: &[(&str, &[&str])], slot: Slot, name: &str) -> Option<String> {
    let norm = normalize_phrase(name);
    phrases(entries, slot)
        .into_iter()
        .find(|p| p.words == norm)
        .map(|p| p.value)
}

pub fn class_of(league: &str) -> Option<String> {
    lookup(CLASSES, Slot::Class, league)
}

pub fn session_of(session: &str) -> Option<String> {
    lookup(SESSIONS, Slot::Session, session)
}
