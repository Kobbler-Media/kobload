//! The koblem review filter applied while RSS scoring.
//!
//! The watch cycle scores every release against every pattern. Retaining all
//! of those for a review that would discard most of them held gigabytes, so the
//! filter now runs at collection time. These tests pin it to what
//! `publish_review` forwards.

use kobload_api::services::want::koblem_review::{review_wanted_title, worth_reviewing};
use kobload_api::watch::{ProcessedRelease, Release};
use nats_jobs::{
    MediaWant, ParsedQuality, QualityRejectReason, RejectReason, ScoringDecision,
    StructuralRejectReason,
};

fn release(title: &str) -> Release {
    Release {
        guid: format!("guid-{title}"),
        title: title.to_string(),
        file_name: None,
        size: 1_000_000_000,
        indexer: "test".to_string(),
        protocol: "torrent".to_string(),
        download_url: None,
        magnet_url: None,
        info_url: None,
        publish_date: "2026-10-12T00:00:00Z".to_string(),
        category_ids: Vec::new(),
        info_hash: None,
        files: None,
        seeders: Some(10),
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

fn processed(title: &str) -> ProcessedRelease {
    ProcessedRelease {
        release: release(title).into(),
        decision: ScoringDecision::NoMatch,
        reject_reasons: Vec::new(),
        match_score: None,
        matched_pattern: None,
        matched_target: None,
        parsed_quality: None,
        quality_score: None,
        quality_rejected: false,
        quality_reject_reasons: Vec::new(),
        quality_breakdown: None,
        structural_rejected: false,
        structural_reject_reason: None,
        total_score: 0.0,
        episode_key: None,
        is_preferred_protocol: false,
        structural_bonus: 0.0,
        seeder_bonus: 0.0,
        freshness_bonus: 0.0,
        context_flags: Vec::new(),
    }
}

fn movie_want(title: &str) -> MediaWant {
    MediaWant::Movie {
        title: title.into(),
        year: Some(2024),
        identifiers: None,
        alternate_titles: None,
        quality_profile_id: None,
        quality_overrides: None,
        release_date: None,
        in_library: None,
        library_video_height: None,
    }
}

fn wanted() -> String {
    review_wanted_title(Some(&movie_want("Dune Part Two")))
}

#[test]
fn wanted_title_comes_from_the_media_want() {
    assert_eq!(wanted(), "Dune Part Two");
    assert_eq!(review_wanted_title(None), "");
}

#[test]
fn unrelated_release_is_not_kept() {
    let pr = processed("Law and Order SVU S28E02 Midway 1080p WEB H264-FLUX");
    assert!(!worth_reviewing(&pr, &wanted()));
}

#[test]
fn related_no_match_is_kept() {
    // No-matches with title overlap are exactly what koblem is there to rescue.
    let pr = processed("Dune.Part.Two.2024.1080p.WEB-DL.DDP5.1.H.264-FLUX");
    assert!(worth_reviewing(&pr, &wanted()));
}

#[test]
fn cam_source_is_not_kept() {
    let mut pr = processed("Dune.Part.Two.2024.CAM.x264");
    pr.parsed_quality = Some(ParsedQuality {
        source: Some("cam".to_string()),
        ..Default::default()
    });
    assert!(!worth_reviewing(&pr, &wanted()));
}

#[test]
fn hard_structural_reject_is_not_kept() {
    let mut pr = processed("Dune.Part.Two.2024.1080p.WEB-DL-FLUX");
    pr.decision = ScoringDecision::Rejected;
    pr.structural_rejected = true;
    pr.structural_reject_reason = Some(StructuralRejectReason::YearMismatch);
    pr.reject_reasons = vec![RejectReason::Structural(
        StructuralRejectReason::YearMismatch,
    )];
    assert!(!worth_reviewing(&pr, &wanted()));
}

#[test]
fn hard_quality_reject_is_not_kept_but_soft_is() {
    let mut pr = processed("Dune.Part.Two.2024.1080p.WEB-DL-FLUX");
    pr.quality_rejected = true;
    pr.quality_reject_reasons = vec![QualityRejectReason::RejectedLanguage];
    assert!(!worth_reviewing(&pr, &wanted()));

    pr.quality_reject_reasons = vec![QualityRejectReason::BelowMinResolution];
    assert!(worth_reviewing(&pr, &wanted()));
}

#[test]
fn want_without_content_words_keeps_everything() {
    let pr = processed("Some.Unrelated.Release.2024.1080p");
    assert!(worth_reviewing(&pr, ""));
}
