//! A release with no session in its name is taken to be the race, but DHT also
//! carries whole race weekends under bare names like `MotoGP.2026.Round15.Austria`
//! (21-46 GiB, every session). Once canonical sport matching lifted those over
//! the match floor they became candidates for the Race want. Single races top
//! out around 15 GiB at 1080p and 32 GiB at 2160p in the capture; the packs
//! start at 17 GiB with no resolution in the name.

use kobload_api::media::match_targets_from_media_want;
use kobload_api::services::want::{
    ScoringConfig, SearchContext, score_release, structural_reject_from_want,
};
use kobload_api::watch::Release;
use nats_jobs::{MediaWant, ScoringDecision, StructuralRejectReason};

const GIB: i64 = 1 << 30;

fn release(title: &str, size: i64) -> Release {
    Release {
        guid: format!("guid-{title}-{size}"),
        title: title.to_string(),
        file_name: None,
        size,
        indexer: "DHT".to_string(),
        protocol: "torrent".to_string(),
        download_url: None,
        magnet_url: None,
        info_url: None,
        publish_date: "2026-10-12T00:00:00Z".to_string(),
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

fn race_want(league: &str, round: u32, location: &str) -> MediaWant {
    MediaWant::SportEvent {
        league_name: league.to_string(),
        season: "2026".to_string(),
        round: Some(round),
        location: Some(location.to_string()),
        session_type: Some("Race".to_string()),
        event_title: None,
        event_date: None,
        quality_profile_id: None,
        quality_overrides: None,
        in_library: None,
        library_video_height: None,
    }
}

fn check(title: &str, gib: f64, want: &MediaWant, pack: bool) {
    let release = release(title, (gib * GIB as f64) as i64);
    let expected = pack.then_some(StructuralRejectReason::SessionMissing);
    assert_eq!(
        structural_reject_from_want(&release, want, SearchContext::Rss),
        expected,
        "{title} at {gib} GiB, search entry point"
    );

    let targets = match_targets_from_media_want(want);
    let config = ScoringConfig::new(&targets, &[], 0.75, None, None).with_media_want(want);
    let scored = score_release(&release, &config);
    assert!(
        scored.match_score.is_some(),
        "{title} should match {want:?}"
    );
    assert_eq!(
        scored.structural_reject_reason, expected,
        "{title} at {gib} GiB, RSS entry point"
    );
    assert_eq!(
        scored.decision == ScoringDecision::Candidate,
        !pack,
        "{title} at {gib} GiB"
    );
}

#[test]
fn bare_weekend_packs_are_not_the_race() {
    let austria = race_want("MotoGP", 15, "Austria GP");
    check("MotoGP.2026.Round15.Austria", 44.1, &austria, true);
    check("MotoGP.2026.Round15.Austria", 22.6, &austria, true);
    check(
        "MotoGP.2026.Round15.Austria.1080p.WEB-DL",
        20.0,
        &austria,
        true,
    );
    check(
        "MotoGP.2026.Round15.Austria.720p.WEB-DL",
        11.0,
        &austria,
        true,
    );
}

#[test]
fn race_sized_releases_without_a_session_are_still_the_race() {
    let austria = race_want("MotoGP", 15, "Austria GP");
    check("MotoGP.2026.Round15.Austria", 4.5, &austria, false);
    check(
        "MotoGP.2026.Round15.Austria.1080p.WEB-DL",
        14.7,
        &austria,
        false,
    );
    check(
        "MotoGP.2026.Round15.Austria.720p.WEB-DL",
        3.7,
        &austria,
        false,
    );
    check(
        "MotoGP.2026.Round15.Austria.2160p.WEB-DL",
        32.2,
        &austria,
        false,
    );
}

#[test]
fn size_does_not_matter_when_the_release_names_the_race() {
    let austria = race_want("MotoGP", 15, "Austria GP");
    check(
        "MotoGP.2026.Round15.Austria.Race.1080p.WEB-DL",
        44.0,
        &austria,
        false,
    );
}
