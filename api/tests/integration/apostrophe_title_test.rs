//! Indexers mangle the apostrophe in "Grey's Anatomy" in several ways: an
//! HTML entity that lost its ampersand, a backslash escape, or a space or
//! underscore where it was. Every matching path has to see these as the same
//! title as "Greys".

use kobload_api::services::want::koblem_review::has_title_overlap;
use kobload_api::services::want::{ScoringConfig, score_release};
use kobload_api::watch::Release;
use nats_jobs::{MatchTarget, MediaWant, ScoringDecision};

const MANGLED: &[&str] = &[
    "Grey#039s.Anatomy.S23E01.Collide.1080p.DSNP.WEB-DL.DD+5.1.H.264-playWEB",
    "Grey&#039;s.Anatomy.S23E01.Collide.1080p.DSNP.WEB-DL.DD+5.1.H.264-playWEB",
    "Grey s.Anatomy.S23E01.Collide.1080p.DSNP.WEB-DL.DD+5.1.H.264-playWEB",
    "Grey\\'s Anatomy S23E01 Collide 1080p DSNP WEB-DL DD 5 1 H 264-playWEB",
    "Grey_s.Anatomy.S23E01.Collide.720p.DSNP.WEB-DL.DD_5.1.H.264-playWEB",
];

fn release(title: &str) -> Release {
    serde_json::from_value(serde_json::json!({
        "guid": title,
        "title": title,
        "size": 2_000_000_000i64,
        "publish_date": "2026-10-12T00:00:00Z",
        "category_ids": [],
        "indexer": "test",
        "protocol": "usenet",
        "raw_data": {},
    }))
    .unwrap()
}

fn want() -> MediaWant {
    MediaWant::TvEpisode {
        show_name: "Grey's Anatomy".to_string(),
        season: "23".to_string(),
        episode: "1".to_string(),
        year: None,
        air_date: None,
        identifiers: None,
        alternate_titles: None,
        episode_title: None,
        quality_profile_id: None,
        quality_overrides: None,
    }
}

fn assert_candidate(config: &ScoringConfig, how: &str) {
    for title in MANGLED {
        let scored = score_release(&release(title), config);
        assert_eq!(
            (scored.decision, scored.reject_reasons.clone()),
            (ScoringDecision::Candidate, vec![]),
            "{how}: {title:?} scored {:?}",
            scored.match_score
        );
    }
}

#[test]
fn mangled_apostrophes_match_targets() {
    let targets = vec![MatchTarget {
        id: "media".to_string(),
        target: "Grey's Anatomy S23E01".to_string(),
        min_score: None,
        semantic_target: None,
    }];
    let mw = want();
    assert_candidate(
        &ScoringConfig::new(&targets, &[], 0.75, None, None).with_media_want(&mw),
        "targets",
    );
}

#[test]
fn mangled_apostrophes_match_patterns() {
    let patterns = vec!["Grey's Anatomy S23E01".to_string()];
    let mw = want();
    for mode in ["fuzzy", "any", "all"] {
        assert_candidate(
            &ScoringConfig::new(&[], &patterns, 0.75, None, None)
                .with_match_mode(mode)
                .with_media_want(&mw),
            mode,
        );
    }
}

#[test]
fn mangled_apostrophes_overlap_the_wanted_title_for_review() {
    for title in MANGLED {
        assert!(has_title_overlap(title, "Grey's Anatomy"), "{title:?}");
    }
}
