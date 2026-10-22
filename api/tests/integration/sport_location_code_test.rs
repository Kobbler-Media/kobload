//! Some sport wants carry the host country as an ISO 3166 code ("DE", "ES")
//! where releases spell the country out. The location gate has to treat the
//! code as that country, but only when the whole location is the code: "IT"
//! or "IN" inside a title are words, not countries.

use kobload_api::media::match_targets_from_media_want;
use kobload_api::services::want::{ScoringConfig, score_release};
use kobload_api::watch::{ProcessedRelease, Release};
use nats_jobs::{MediaWant, ScoringDecision, StructuralRejectReason};

fn release(title: &str) -> Release {
    serde_json::from_value(serde_json::json!({
        "guid": title,
        "title": title,
        "size": 2_000_000_000i64,
        "publish_date": "2026-09-01T12:00:00Z",
        "category_ids": [5000, 5060],
        "indexer": "test",
        "protocol": "torrent",
        "raw_data": {},
    }))
    .unwrap()
}

fn motogp(round: u32, location: &str) -> MediaWant {
    MediaWant::SportEvent {
        league_name: "MotoGP".to_string(),
        season: "2026".to_string(),
        round: Some(round),
        location: Some(location.to_string()),
        session_type: Some("Sprint Race".to_string()),
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

#[test]
fn a_country_code_matches_the_country_it_stands_for() {
    for (want, title) in [
        (
            motogp(11, "DE"),
            "www.UIndex.org    -    MotoGP 2026 Germany Sprint Race 720p WEB H264-JFF",
        ),
        (
            motogp(13, "ES"),
            "www.UIndex.org    -    MotoGP 2026 Round13 Spain Aragon Sprint WEB-DL 1080p H264 English-MWR",
        ),
        (
            motogp(13, "es"),
            "MotoGP.2026.Round13.Spain.Aragon.Sprint.WEB-DL.1080p",
        ),
    ] {
        let scored = score(title, &want);
        assert_eq!(
            (scored.decision, scored.structural_reject_reason),
            (ScoringDecision::Candidate, None),
            "{title}"
        );
    }
}

#[test]
fn a_country_code_still_rejects_other_countries() {
    for (want, title) in [
        (
            motogp(11, "DE"),
            "MotoGP.2026.Round11.Austria.Sprint.WEB-DL.1080p",
        ),
        (
            motogp(15, "IT"),
            "MotoGP.2026.Round15.San.Marino.Misano.Sprint.WEB-DL.1080p",
        ),
    ] {
        assert_eq!(
            score(title, &want).structural_reject_reason,
            Some(StructuralRejectReason::LocationMismatch),
            "{title}"
        );
    }
}
