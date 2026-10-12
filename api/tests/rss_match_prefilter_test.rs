//! The RSS loop skips full scoring for release × pattern pairs that don't
//! match. That's only safe while `release_matches` agrees exactly with the
//! match gate inside `score_release`.

use kobload_api::quality::parse_quality;
use kobload_api::services::want::{ScoringConfig, release_matches, score_release};
use kobload_api::watch::Release;
use nats_jobs::MatchTarget;

fn release(title: &str) -> Release {
    Release {
        guid: format!("guid-{title}"),
        title: title.to_string(),
        file_name: None,
        size: 1_000_000_000,
        indexer: "test".to_string(),
        protocol: "usenet".to_string(),
        download_url: None,
        magnet_url: None,
        info_url: None,
        publish_date: "2026-10-12T00:00:00Z".to_string(),
        category_ids: Vec::new(),
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

fn target(text: &str) -> MatchTarget {
    MatchTarget {
        id: text.to_string(),
        target: text.to_string(),
        min_score: None,
        semantic_target: None,
    }
}

const TITLES: &[&str] = &[
    "Law and Order SVU S28E02 Midway 1080p WEB H264-FLUX",
    "Law.and.Order.Special.Victims.Unit.S28E02.720p.HDTV.x264-SYNCOPY",
    "Law.and.Order.S24E02.1080p.WEB.h264-ETHEL",
    "Formula1.2026.Round19.United.States.Race.1080p.WEB-DL",
    "MotoGP.2026.Round18.Australia.Sprint.1080p.WEB.h264-VERUM",
    "Dune.Part.Two.2024.2160p.UHD.BluRay.REMUX.HDR10+.HEVC-GRP",
    "Some.Random.Documentary.2025.1080p.WEB-DL-NOGRP",
];

fn assert_agrees(config: &ScoringConfig) {
    for title in TITLES {
        let r = release(title);
        assert_eq!(
            release_matches(&r, config),
            score_release(&r, config).match_score.is_some(),
            "pre-check disagrees with score_release for {title:?}"
        );
    }
}

#[test]
fn agrees_with_score_release_for_targets() {
    let targets = vec![
        target("Law and Order Special Victims Unit S28E02"),
        target("Law and Order SVU S28E02"),
    ];
    assert_agrees(&ScoringConfig::new(&targets, &[], 0.75, None, None));
    assert_agrees(&ScoringConfig::new(&targets, &[], 0.5, None, None));

    let sport = vec![target("Formula 1 2026 Round 19 United States Race")];
    assert_agrees(&ScoringConfig::new(&sport, &[], 0.75, None, None));
}

#[test]
fn agrees_with_score_release_for_patterns() {
    let patterns = vec!["Dune Part Two".to_string(), "MotoGP 2026".to_string()];
    assert_agrees(&ScoringConfig::new(&[], &patterns, 0.75, None, None));
    assert_agrees(
        &ScoringConfig::new(&[], &patterns, 0.75, None, None).with_match_mode("exact"),
    );
}

#[test]
fn nothing_to_match_never_matches() {
    let config = ScoringConfig::new(&[], &[], 0.75, None, None);
    assert_agrees(&config);
    assert!(!release_matches(&release(TITLES[0]), &config));
}

#[test]
fn matching_release_is_found() {
    let targets = vec![target("Law and Order SVU S28E02")];
    let config = ScoringConfig::new(&targets, &[], 0.75, None, None);
    assert!(release_matches(&release(TITLES[0]), &config));
    assert!(!release_matches(&release(TITLES[6]), &config));
}

#[test]
fn score_release_reports_the_title_parse() {
    // score_release now reuses score_quality's parse instead of parsing again.
    let targets = vec![target("Dune Part Two")];
    let config = ScoringConfig::new(&targets, &[], 0.75, None, None);
    for title in TITLES {
        let scored = score_release(&release(title), &config);
        assert_eq!(
            serde_json::to_value(scored.parsed_quality.unwrap()).unwrap(),
            serde_json::to_value(parse_quality(title)).unwrap(),
            "parsed_quality differs for {title:?}"
        );
    }
}
