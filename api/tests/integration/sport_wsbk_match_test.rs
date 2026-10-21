//! WorldSBK releases were never matched to their wants. Kobbler builds the want
//! from TheSportsDB ("SBK 2026 R12 Spain Race 1") while the scene names the file
//! `WSBK.2026.Round12.Spain.Jerez.Race.One...`: the round, the session number and
//! often the place were spelt differently, the fuzzy match scored ~0.62 against a
//! 0.75 floor, and the whole Jerez weekend was missed while sitting in the RSS
//! feeds. These titles are the real releases from that weekend's capture.

use kobload_api::media::match_targets_from_media_want;
use kobload_api::parsing::canonical_sport_text;
use kobload_api::services::want::{ScoringConfig, score_release};
use kobload_api::watch::{ProcessedRelease, Release};
use nats_jobs::{MediaWant, ScoringDecision, StructuralRejectReason};

const RACE_ONE: &str = "WSBK.2026.Round12.Spain.Jerez.Race.One.WEB-DL.1080p.H264.English-MWR";
const RACE_TWO: &str = "WSBK.2026.Round12.Spain.Jerez.Race.Two.WEB-DL.1080p.H264.English-MWR";
const SUPERPOLE_RACE: &str =
    "WSBK.2026.Round12.Spain.Jerez.Superpole.Race.WEB-DL.1080p.H264.English-MWR";
const SUPERPOLE: &str = "WSBK.2026.Round12.Spain.Jerez.Superpole.WEB-DL.1080p.H264.English-MWR";
const FP1: &str = "WSBK.2026.Round12.Spain.Jerez.FP1.WEB-DL.1080p.H264.English-MWR";
const LAST_ROUND: &str =
    "WSBK.2026.Round11.Portugal.Estoril.Race.Two.WEB-DL.1080p.H264.English-MWR";

fn release(title: &str) -> Release {
    Release {
        guid: format!("guid-{title}"),
        title: title.to_string(),
        file_name: None,
        size: 2_000_000_000,
        indexer: "test".to_string(),
        protocol: "usenet".to_string(),
        download_url: None,
        magnet_url: None,
        info_url: None,
        publish_date: "2026-10-19T13:15:00Z".to_string(),
        category_ids: vec![5000, 5060],
        info_hash: None,
        files: None,
        seeders: None,
        peers: None,
        leechers: None,
        grabs: None,
        votes: None,
        indexer_id: None,
        indexer_source_id: None,
        raw_data: serde_json::json!({}),
        external_ids: Vec::new(),
        media: None,
    }
}

fn sbk_want(round: u32, location: &str, session: &str) -> MediaWant {
    MediaWant::SportEvent {
        league_name: "SBK".to_string(),
        season: "2026".to_string(),
        round: Some(round),
        location: Some(location.to_string()),
        session_type: Some(session.to_string()),
        event_title: None,
        event_date: None,
        quality_profile_id: None,
        quality_overrides: None,
        in_library: None,
        library_video_height: None,
    }
}

fn score(title: &str, want: &MediaWant) -> ProcessedRelease {
    let targets = match_targets_from_media_want(want);
    let config = ScoringConfig::new(&targets, &[], 0.75, None, None).with_media_want(want);
    score_release(&release(title), &config)
}

fn assert_candidate(title: &str, want: &MediaWant) {
    let scored = score(title, want);
    assert!(
        scored.match_score.is_some(),
        "{title} should match its want {want:?}"
    );
    assert_eq!(scored.structural_reject_reason, None, "{title}");
    assert_eq!(scored.decision, ScoringDecision::Candidate, "{title}");
}

fn assert_not_candidate(title: &str, want: &MediaWant) {
    assert_ne!(
        score(title, want).decision,
        ScoringDecision::Candidate,
        "{title} must not be grabbed for {want:?}"
    );
}

#[test]
fn canonical_sport_text_lines_want_and_release_up() {
    assert_eq!(
        canonical_sport_text(RACE_ONE),
        "sbk 2026 r12 spain jerez race 1 web dl 1080p h264 english mwr"
    );
    assert_eq!(
        canonical_sport_text("SBK 2026 R12 Spain Race 1"),
        "sbk 2026 r12 spain race 1"
    );
    assert_eq!(canonical_sport_text("Round 7 Race Two"), "r7 race 2");
    assert_eq!(canonical_sport_text("R07"), "r7");
    assert_eq!(canonical_sport_text("Free Practice 1"), "fp1");
    assert_eq!(canonical_sport_text("Practice.One"), "fp1");
    assert_eq!(canonical_sport_text("FP1"), "fp1");
    assert_eq!(canonical_sport_text("Qualifying Two"), "q2");
    assert_eq!(canonical_sport_text("Warm.Up"), "warmup");
    assert_eq!(canonical_sport_text("Great.Britain"), "uk");
    assert_eq!(canonical_sport_text("Czech.Republic"), "czech");
    assert_eq!(canonical_sport_text("Czechia"), "czech");
    assert_eq!(canonical_sport_text("Spanish"), "spain");
    assert_eq!(canonical_sport_text("WorldSBK"), "sbk");
    assert_eq!(canonical_sport_text("Formula1"), "f1");
    assert_eq!(canonical_sport_text("Formula 1"), "f1");
    // Session words without a number, and non-round "r" words, are untouched.
    assert_eq!(canonical_sport_text("Superpole Race"), "superpole race");
    assert_eq!(canonical_sport_text("R2D2 Rd"), "r2d2 rd");
}

#[test]
fn jerez_releases_match_their_wants() {
    assert_candidate(RACE_ONE, &sbk_want(12, "Spain", "Race 1"));
    assert_candidate(RACE_TWO, &sbk_want(12, "Spain", "Race 2"));
    assert_candidate(SUPERPOLE_RACE, &sbk_want(12, "Spain", "Superpole Race"));
    assert_candidate(SUPERPOLE, &sbk_want(12, "Spain", "Superpole"));
    assert_candidate(FP1, &sbk_want(12, "Spain", "Free Practice 1"));
}

#[test]
fn a_nationality_matches_the_country() {
    assert_candidate(RACE_ONE, &sbk_want(12, "Spanish", "Race 1"));
}

#[test]
fn alternative_country_names_match() {
    assert_candidate(
        "WSBK.2026.Round08.Great.Britain.Race.One.WEB-DL.1080p.H264.English-MWR",
        &sbk_want(8, "UK", "Race 1"),
    );
    assert_candidate(
        "WSBK 2026 Round05 Czech Republic Race Two",
        &sbk_want(5, "Czechia", "Race 2"),
    );
}

#[test]
fn other_sessions_of_the_weekend_are_not_grabbed() {
    let race_one = sbk_want(12, "Spain", "Race 1");
    for title in [RACE_TWO, SUPERPOLE_RACE, SUPERPOLE, FP1] {
        assert_not_candidate(title, &race_one);
    }
    assert_not_candidate(RACE_ONE, &sbk_want(12, "Spain", "Superpole Race"));
}

#[test]
fn other_rounds_and_places_are_not_grabbed() {
    assert_eq!(
        score(LAST_ROUND, &sbk_want(12, "Spain", "Race 2")).structural_reject_reason,
        Some(StructuralRejectReason::RoundMismatch)
    );
    assert_eq!(
        score(RACE_ONE, &sbk_want(12, "Portugal", "Race 1")).structural_reject_reason,
        Some(StructuralRejectReason::LocationMismatch)
    );
}

/// Nothing in scoring checks the league, so once the round, place and session
/// line up the league token is all that separates the classes on one weekend.
#[test]
fn other_classes_on_the_same_weekend_are_not_grabbed() {
    assert_not_candidate(
        "WSSP.2026.Round12.Spain.Jerez.Race.One.WEB-DL.1080p.H264.English-MWR",
        &sbk_want(12, "Spain", "Race 1"),
    );
    let moto2 = MediaWant::SportEvent {
        league_name: "Moto2".to_string(),
        season: "2026".to_string(),
        round: Some(18),
        location: Some("Australia".to_string()),
        session_type: Some("Race".to_string()),
        event_title: None,
        event_date: None,
        quality_profile_id: None,
        quality_overrides: None,
        in_library: None,
        library_video_height: None,
    };
    assert_not_candidate(
        "MotoGP.2026.Round18.Australia.Race.1080p.WEB.h264-VERUM",
        &moto2,
    );
}

/// What Kobbler sent before it learnt to strip the sponsor and the "Superpole"
/// out of the location. The location gate is right to refuse this; the fix for
/// it is on Kobbler's side.
#[test]
fn a_sponsor_round_name_is_still_a_location_mismatch() {
    assert_eq!(
        score(RACE_ONE, &sbk_want(12, "Pirelli Spanish Round", "Race 1")).structural_reject_reason,
        Some(StructuralRejectReason::LocationMismatch)
    );
}
