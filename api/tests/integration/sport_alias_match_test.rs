//! Sport wants from a Kobbler that knows every name an event goes by: the
//! venue, its town and region, the country and its codes, the league's other
//! names and the organiser's event names. A release is this want when it
//! names the class, a place (or the round, or the date) and the session, and
//! nothing in it names another one.
//!
//! The release titles are real ones from the capture and the DHT index,
//! except where a test says otherwise.

use kobload_api::media::match_targets_from_media_want;
use kobload_api::services::want::{
    ScoringConfig, SearchContext, score_release, structural_reject_from_want,
};
use kobload_api::watch::{ProcessedRelease, Release};
use nats_jobs::{MediaWant, ScoringDecision, StructuralRejectReason};
use serde_json::{Value, json};

fn release(title: &str, published: &str) -> Release {
    serde_json::from_value(json!({
        "guid": title,
        "title": title,
        "size": 2_000_000_000i64,
        "publish_date": published,
        "category_ids": [5000, 5060],
        "indexer": "test",
        "protocol": "torrent",
        "raw_data": {},
    }))
    .unwrap()
}

fn want(base: Value, meta: Value) -> MediaWant {
    let mut v = base;
    v["kind"] = "sport_event".into();
    v["quality_profile_id"] = Value::Null;
    v["quality_overrides"] = Value::Null;
    for (k, val) in meta.as_object().unwrap() {
        v[k] = val.clone();
    }
    serde_json::from_value(v).unwrap()
}

fn americas(session: &str, date: &str) -> MediaWant {
    want(
        json!({"league_name": "MotoGP", "season": "2026", "round": 3, "location": "USA",
               "session_type": session, "event_date": date}),
        json!({
            "venue": "Circuit of the Americas", "city": "Austin", "region": "Texas",
            "country": "United States", "country_iso": "US", "class": "MotoGP",
            "location_aliases": ["USA", "Circuit of the Americas", "COTA", "Americas", "Austin",
                                 "Texas", "United States", "United States of America",
                                 "America", "US", "American"],
            "event_aliases": ["Red Bull Grand Prix of The United States",
                              "Grand Prix of the Americas"],
            "session_start": format!("{date}T19:00:00+00:00"),
        }),
    )
}

fn sbk_aragon(session: &str, date: &str) -> MediaWant {
    want(
        json!({"league_name": "SBK", "season": "2026", "round": 6, "location": "ES",
               "session_type": session, "event_date": date}),
        json!({
            "venue": "MotorLand Aragón", "city": "Alcañiz", "region": "Aragon",
            "country": "Spain", "country_iso": "ES", "class": "SBK",
            "location_aliases": ["ES", "MotorLand Aragón", "Aragón", "Alcañiz", "Aragon",
                                 "Spain", "ESP", "Spanish"],
            "event_aliases": ["Aragon Round", "SBK 2026-05-30 Aragon Round Race 1"],
            "league_aliases": ["WSB", "WSBK", "World Superbike"],
        }),
    )
}

fn motogp_aragon(session: &str, date: &str) -> MediaWant {
    want(
        json!({"league_name": "MotoGP", "season": "2026", "round": 13, "location": "ES",
               "session_type": session, "event_date": date}),
        json!({
            "venue": "MotorLand Aragón", "city": "Alcañiz", "region": "Aragon",
            "country": "Spain", "country_iso": "ES", "class": "MotoGP",
            "location_aliases": ["ES", "MotorLand Aragón", "Aragón", "Alcañiz", "Aragon",
                                 "Spain", "ESP", "Spanish"],
            "event_aliases": ["Gran Premio GoPro de Aragón", "Gran Premio de Aragón"],
        }),
    )
}

fn germany(league: &str, session: &str, date: &str) -> MediaWant {
    want(
        json!({"league_name": league, "season": "2026", "round": 11, "location": "DE",
               "session_type": session, "event_date": date}),
        json!({
            "venue": "Sachsenring", "city": "Hohenstein-Ernstthal", "region": "Saxony",
            "country": "Germany", "country_iso": "DE", "class": league,
            "location_aliases": ["DE", "Sachsenring", "Hohenstein-Ernstthal", "Chemnitz",
                                 "Saxony", "Germany", "DEU", "German"],
            "event_aliases": ["Liqui Moly Motorrad Grand Prix Deutschland"],
        }),
    )
}

fn japan(league: &str) -> MediaWant {
    want(
        json!({"league_name": league, "season": "2026", "round": 16, "location": "Japan",
               "session_type": "Race", "event_date": "2026-10-04"}),
        json!({
            "venue": "Mobility Resort Motegi", "city": "Motegi", "region": "Tochigi",
            "country": "Japan", "country_iso": "JP", "class": league,
            "location_aliases": ["Japan", "Mobility Resort Motegi", "Motegi", "Tochigi",
                                 "JP", "JPN", "Japanese"],
        }),
    )
}

fn score(title: &str, published: &str, want: &MediaWant) -> ProcessedRelease {
    let targets = match_targets_from_media_want(want);
    let config = ScoringConfig::new(&targets, &[], 0.75, None, None).with_media_want(want);
    score_release(&release(title, published), &config)
}

/// Both entry points: the RSS/search scoring and the structural check the
/// backlog and the match handler run on their own.
fn assert_grabbed(title: &str, published: &str, want: &MediaWant) {
    let scored = score(title, published, want);
    assert!(scored.match_score.is_some(), "{title} should match");
    assert_eq!(
        (scored.decision, scored.structural_reject_reason),
        (ScoringDecision::Candidate, None),
        "{title}"
    );
    assert_eq!(
        structural_reject_from_want(
            &release(title, published),
            want,
            SearchContext::Rss,
            Default::default()
        ),
        None,
        "{title}, backlog entry point"
    );
}

fn assert_not_grabbed(title: &str, published: &str, want: &MediaWant) {
    assert_ne!(
        score(title, published, want).decision,
        ScoringDecision::Candidate,
        "{title} must not be grabbed"
    );
}

#[test]
fn americas_gp_releases_named_after_the_circuit_or_town_are_the_race() {
    let race = americas("Race", "2026-03-29");
    for title in [
        "USA COTA MotoGP Race [HDTV-1080p][AAC 2.0][x264]-S2026E30-Kalimero.mp4",
        // Synthetic: the shapes MWR and VERUM use, named after the town and
        // the circuit instead of the country.
        "MotoGP.2026.Round03.Austin.Race.WEB-DL.1080p.H264.English-MWR",
        "MotoGP.2026.Americas.Race.1080p.WEB.h264-VERUM",
        "MotoGP.2026.Grand.Prix.of.the.Americas.1080p.WEB.h264-VERUM",
    ] {
        assert_grabbed(title, "2026-03-29T21:00:00Z", &race);
    }
}

#[test]
fn americas_gp_other_sessions_and_rounds_are_not_the_race() {
    let race = americas("Race", "2026-03-29");
    for title in [
        "www.UIndex.org    -    MotoGP 2026 Round03 USA Sprint WEB-DL 1080p H264 English-MWR",
        "www.UIndex.org    -    MotoGP 2026 USA Qualifying Two 720p WEB H264-JFF",
        "www.UIndex.org    -    MotoGP 2026 Round04 Spain Jerez Race WEB-DL 1080p H264 English-MWR",
        "Moto2.USA.Carrera.2026.720p.Spa.EveHQ.mkv",
    ] {
        assert_not_grabbed(title, "2026-03-29T21:00:00Z", &race);
    }
}

#[test]
fn an_sbk_round_named_only_after_its_circuit_is_matched() {
    let race_one = sbk_aragon("Race 1", "2026-05-30");
    for title in [
        "WSBK.2026.Round06.Aragon.Race.One.WEB-DL.1080p.H264.English-MWR",
        "WSBK.2026.Round06.Spain.Aragon.Race.One.WEB-DL.1080p.H264.English-MWR",
        "WSBK 2026 MotorLand Aragon Race 1 1080p WEB h264-GRP",
    ] {
        assert_grabbed(title, "2026-05-30T18:00:00Z", &race_one);
    }
    for title in [
        "WSBK.2026.Round06.Aragon.Race.Two.WEB-DL.1080p.H264.English-MWR",
        "WSBK.2026.Round12.Spain.Jerez.Race.One.WEB-DL.1080p.H264.English-MWR",
        "WSSP.2026.Round06.Aragon.Race.One.WEB-DL.1080p.H264.English-MWR",
    ] {
        assert_not_grabbed(title, "2026-05-30T18:00:00Z", &race_one);
    }
}

#[test]
fn motogp_aragon_releases_dated_or_in_french_are_matched() {
    let sprint = motogp_aragon("Sprint Race", "2026-08-29");
    for title in [
        "MotoGP of Aragon Sprint 29.08.2026.mkv",
        "MotoGP.La.Course.Sprint.GP.Aragon.29.08.2026.VFF.1080p.WEBRip.AAC.2.0.x264-DN55.mp4",
        "www.UIndex.org    -    MotoGP 2026 Round13 Spain Aragon Sprint WEB-DL 1080p H264 English-MWR",
    ] {
        assert_grabbed(title, "2026-08-29T20:00:00Z", &sprint);
    }
    let race = motogp_aragon("Race", "2026-08-30");
    for title in [
        "MotoGP of Aragon 30.08.2026",
        "MotoGP.La.Course.GP.Aragon.30.08.2026.VFF.1080p.WEBRip.AAC.2.0.x264-DN55.mp4",
        "www.UIndex.org    -    MotoGP 2026 Aragon 720p WEB H264-JFF",
    ] {
        assert_grabbed(title, "2026-08-30T20:00:00Z", &race);
    }
    assert_not_grabbed(
        "MotoGP of Aragon Sprint 29.08.2026.mkv",
        "2026-08-29T20:00:00Z",
        &race,
    );
}

#[test]
fn a_league_alias_satisfies_the_league_token() {
    let race_one = sbk_aragon("Race 1", "2026-05-30");
    // Synthetic: "WSB" is only known from the league's Sportarr aliases.
    assert_grabbed(
        "WSB.2026.Round06.Aragon.Race.One.1080p.WEB.h264-GRP",
        "2026-05-30T18:00:00Z",
        &race_one,
    );
    assert_grabbed(
        "World.Superbike.2026.Round06.Aragon.Race.One.1080p.WEB.h264-GRP",
        "2026-05-30T18:00:00Z",
        &race_one,
    );
    assert_not_grabbed(
        "2026.Round06.Aragon.Race.One.1080p.WEB.h264-GRP",
        "2026-05-30T18:00:00Z",
        &race_one,
    );
}

/// "DE", "ES" and "ITA" are in a want's place names because some data names
/// the country by its code, and releases use the same letters as language
/// tags and French "de".
#[test]
fn a_country_code_is_never_read_from_a_language_tag() {
    let race = germany("MotoGP", "Race", "2026-07-12");
    assert_not_grabbed(
        "MotoGP.Grand.Prix.De.Saint.Marin.2026.Course+Podium.VFF.1080p.WEB.EAC3.x264-PiXeL.mkv",
        "2026-09-13T20:00:00Z",
        &race,
    );
    assert_not_grabbed(
        "MotoGP.2026.Race.DE.1080p.WEB.h264-GRP",
        "2026-07-12T20:00:00Z",
        &race,
    );
    let sprint = motogp_aragon("Sprint Race", "2026-08-29");
    assert_not_grabbed(
        "MotoGP.2026.Sprint.Race.ES.1080p.WEB.h264-GRP",
        "2026-08-29T20:00:00Z",
        &sprint,
    );
    assert_not_grabbed(
        "MotoGP.2026.Sprint.ESP.1080p.WEB.h264-GRP",
        "2026-08-29T20:00:00Z",
        &sprint,
    );
    assert_grabbed(
        "MotoGP of German Sprint 11.07.2026.mkv",
        "2026-07-11T20:00:00Z",
        &germany("MotoGP", "Sprint Race", "2026-07-11"),
    );
}

/// Synthetic titles in the shapes the supplement's same-weekend decoys use.
#[test]
fn the_class_keeps_moto2_and_motogp_apart() {
    let moto2 = japan("Moto2");
    assert_not_grabbed(
        "MotoGP.2026.Round16.Japan.Race.1080p.WEB.h264-VERUM",
        "2026-10-04T08:00:00Z",
        &moto2,
    );
    assert_grabbed(
        "MotoGP.2026.R16.Japanese.Grand.Prix.Moto2.Race.1080p.mkv",
        "2026-10-04T08:00:00Z",
        &moto2,
    );
    let motogp = japan("MotoGP");
    assert_not_grabbed(
        "MotoGP.2026.R16.Japanese.Grand.Prix.Moto2.Race.1080p.mkv",
        "2026-10-04T08:00:00Z",
        &motogp,
    );
    assert_not_grabbed(
        "Moto3.2026.Round16.Japan.Race.1080p.WEB.h264-VERUM",
        "2026-10-04T08:00:00Z",
        &motogp,
    );
    assert_grabbed(
        "MotoGP.2026.Round16.Japan.Race.1080p.WEB.h264-VERUM",
        "2026-10-04T08:00:00Z",
        &motogp,
    );
}

#[test]
fn a_mismatch_is_reported_for_the_weekends_other_sessions() {
    let race_one = sbk_aragon("Race 1", "2026-05-30");
    let title = "WSBK.2026.Round06.Aragon.Race.Two.WEB-DL.1080p.H264.English-MWR";
    let scored = score(title, "2026-05-31T18:00:00Z", &race_one);
    assert!(scored.match_score.is_some(), "{title}");
    assert_eq!(
        scored.structural_reject_reason,
        Some(StructuralRejectReason::SessionMismatch)
    );
    assert_eq!(
        structural_reject_from_want(
            &release(title, "2026-05-31T18:00:00Z"),
            &race_one,
            SearchContext::Rss,
            Default::default()
        ),
        Some(StructuralRejectReason::SessionMismatch)
    );
}

/// A Kobbler from before the aliases sends none of them. Its wants must be
/// matched exactly as they always were, place-name blind spots included.
#[test]
fn an_old_shape_want_keeps_the_old_matching() {
    let old: MediaWant = serde_json::from_value(json!({
        "kind": "sport_event", "league_name": "SBK", "season": "2026", "round": 6,
        "location": "ES", "session_type": "Race 1", "event_date": "2026-05-30",
        "quality_profile_id": null, "quality_overrides": null,
    }))
    .unwrap();
    let MediaWant::SportEvent { meta, .. } = &old else {
        unreachable!()
    };
    assert!(meta.location_aliases.is_none() && meta.class.is_none());

    let circuit_only = "WSBK.2026.Round06.Aragon.Race.One.WEB-DL.1080p.H264.English-MWR";
    assert_eq!(
        score(circuit_only, "2026-05-30T18:00:00Z", &old).structural_reject_reason,
        Some(StructuralRejectReason::LocationMismatch)
    );
    assert_eq!(
        structural_reject_from_want(
            &release(circuit_only, "2026-05-30T18:00:00Z"),
            &old,
            SearchContext::Rss,
            Default::default()
        ),
        Some(StructuralRejectReason::LocationMismatch)
    );
    assert_grabbed(
        "WSBK.2026.Round06.Spain.Aragon.Race.One.WEB-DL.1080p.H264.English-MWR",
        "2026-05-30T18:00:00Z",
        &old,
    );
    assert_eq!(
        score(
            "MotoGP of Aragon Sprint 29.08.2026.mkv",
            "2026-08-29T20:00:00Z",
            &serde_json::from_value(json!({
                "kind": "sport_event", "league_name": "MotoGP", "season": "2026", "round": 13,
                "location": "ES", "session_type": "Sprint Race", "event_date": "2026-08-29",
                "quality_profile_id": null, "quality_overrides": null,
            }))
            .unwrap(),
        )
        .match_score,
        None
    );
}

/// The watch page's "test a title" check matches a title against a stored
/// pattern without scoring it.
#[test]
fn the_pattern_title_check_reads_the_aliases_too() {
    let now = time::OffsetDateTime::now_utc();
    let want = americas("Race", "2026-03-29");
    let pattern = kobload_api::repositories::watch::WatchPatternRow {
        id: uuid::Uuid::nil(),
        source_system: "kobbler".into(),
        source_id: "americas-race".into(),
        callback_url: String::new(),
        patterns: Vec::new(),
        media_want: Some(serde_json::to_value(&want).unwrap()),
        match_targets: None,
        match_mode: None,
        min_score: None,
        media_type_id: None,
        auto_download: true,
        metadata: None,
        enabled: true,
        upgrade_allowed: false,
        upgrade_until: None,
        created_at: now,
        updated_at: now,
        last_backlog_search_at: None,
        backlog_search_interval_hours: None,
        excluded_releases: None,
        search_not_before: None,
    };
    let check = |title: &str| {
        kobload_api::watch::service::match_release_with_pattern(
            &pattern,
            Some(&want),
            title,
            &Default::default(),
        )
    };
    for title in [
        "MotoGP.2026.Round03.Austin.Race.WEB-DL.1080p.H264.English-MWR",
        "MotoGP.2026.Grand.Prix.of.the.Americas.1080p.WEB.h264-VERUM",
        "USA COTA MotoGP Race [HDTV-1080p][AAC 2.0][x264]-S2026E30-Kalimero.mp4",
    ] {
        assert!(check(title).is_some(), "{title}");
    }
    assert!(check("Moto2.USA.Carrera.2026.720p.Spa.EveHQ.mkv").is_none());
}

/// Team sports have no place or session; the teams are the event, and
/// releases use whichever of a team's names they like.
#[test]
fn team_releases_match_by_any_of_the_teams_names() {
    let fixture = want(
        json!({"league_name": "English Premier League", "season": "2026-2027",
               "event_title": "Manchester United vs Arsenal", "event_date": "2026-09-12"}),
        json!({"teams": [
            {"name": "Manchester United", "short": "MUN", "aliases": ["Man Utd", "Man United"]},
            {"name": "Arsenal", "short": "ARS", "aliases": ["Gunners"]},
        ]}),
    );
    for title in [
        "EPL.2026.Man.Utd.vs.Arsenal.1080p.WEB.h264-GRP",
        "Man United vs Arsenal 2026-2027 720p HDTV x264-GRP",
    ] {
        assert_grabbed(title, "2026-09-12T20:00:00Z", &fixture);
    }
    assert_not_grabbed(
        "EPL.2026.Man.Utd.vs.Chelsea.1080p.WEB.h264-GRP",
        "2026-09-12T20:00:00Z",
        &fixture,
    );
}

/// "Czech" (from "Czechia") is also an audio-language tag, and it sits where
/// releases put the place.
#[test]
fn a_place_that_is_also_a_language_tag_is_read_as_the_place() {
    let race = want(
        json!({"league_name": "Moto3", "season": "2026", "round": 9, "location": "Czechia",
               "session_type": "Race", "event_date": "2026-06-21"}),
        json!({"country": "Czechia", "country_iso": "CZ", "class": "Moto3",
               "venue": "Automotodrom Brno", "city": "Brno",
               "location_aliases": ["Czechia", "Automotodrom Brno", "Brno", "CZ", "CZE",
                                    "Czech", "Czech Republic"]}),
    );
    assert_grabbed(
        "Moto3.2026.Czechia.1080p.WEB.h264-VERUM",
        "2026-06-21T10:44:44Z",
        &race,
    );
}

#[test]
fn a_country_names_the_round_only_when_the_round_is_named_after_it() {
    assert_grabbed(
        "www.UIndex.org    -    MotoGP 2026 Germany Sprint Race 720p WEB H264-JFF",
        "2026-08-20T19:35:53Z",
        &germany("MotoGP", "Sprint Race", "2026-07-11"),
    );
    let aragon = want(
        json!({"league_name": "MotoGP", "season": "2026", "round": 13, "location": "Aragón",
               "session_type": "Race", "event_date": "2026-08-30"}),
        json!({"country": "Spain", "country_iso": "ES", "class": "MotoGP",
               "location_aliases": ["Aragón", "MotorLand Aragón", "Alcañiz", "Aragon", "Spain",
                                    "ES", "ESP", "Spanish"]}),
    );
    assert_grabbed(
        "www.UIndex.org    -    MotoGP 2026 Spain Race 720p WEB H264-JFF",
        "2026-08-31T10:00:00Z",
        &aragon,
    );
    assert_not_grabbed(
        "www.UIndex.org    -    MotoGP 2026 Spain Race 720p WEB H264-JFF",
        "2026-10-20T10:00:00Z",
        &aragon,
    );
}
