//! A book is released under each of its edition titles: Pullman's "Northern
//! Lights" is "The Golden Compass" in the US. Kobbler sends the other titles
//! as `alternate_titles`, and a release under any of them is the book.

use kobload_api::media::match_targets_from_media_want;
use kobload_api::services::want::{
    ScoringConfig, SearchContext, score_release, structural_reject_from_want,
};
use kobload_api::watch::Release;
use nats_jobs::{MediaWant, ScoringDecision};
use serde_json::json;

fn release(title: &str) -> Release {
    serde_json::from_value(json!({
        "guid": title,
        "title": title,
        "size": 2_000_000i64,
        "publish_date": "2026-10-01T12:00:00Z",
        "category_ids": [7020],
        "indexer": "test",
        "protocol": "torrent",
        "raw_data": {},
    }))
    .unwrap()
}

fn northern_lights(kind: &str, alternate_titles: Option<Vec<&str>>) -> MediaWant {
    serde_json::from_value(json!({
        "kind": kind,
        "title": "Northern Lights",
        "author": "Philip Pullman",
        "isbn": null,
        "identifiers": null,
        "series": "His Dark Materials",
        "series_position": "1",
        "alternate_titles": alternate_titles,
    }))
    .unwrap()
}

fn decision(title: &str, want: &MediaWant) -> ScoringDecision {
    let targets = match_targets_from_media_want(want);
    let config = ScoringConfig::new(&targets, &[], 0.75, None, None).with_media_want(want);
    let scored = score_release(&release(title), &config);
    assert_eq!(
        structural_reject_from_want(
            &release(title),
            want,
            SearchContext::Rss,
            Default::default()
        ),
        scored.structural_reject_reason,
        "{title}: the backlog check disagrees with scoring"
    );
    scored.decision
}

const EDITIONS: &[(&str, &str)] = &[
    ("book", "Philip Pullman - The Golden Compass (1995) [EPUB]"),
    ("book", "The.Golden.Compass.Philip.Pullman.epub"),
    (
        "audiobook",
        "Philip Pullman - The Golden Compass [Unabridged] (m4b)",
    ),
];

#[test]
fn a_release_under_an_alternate_title_is_the_book() {
    for (kind, title) in EDITIONS {
        let want = northern_lights(kind, Some(vec!["The Golden Compass"]));
        assert_eq!(
            decision(title, &want),
            ScoringDecision::Candidate,
            "{title}"
        );
    }
}

#[test]
fn without_alternate_titles_only_the_wanted_title_matches() {
    for (kind, title) in EDITIONS {
        let want = northern_lights(kind, None);
        assert_ne!(
            decision(title, &want),
            ScoringDecision::Candidate,
            "{title}"
        );
    }
    assert_eq!(
        decision(
            "Philip Pullman - Northern Lights (1995) [EPUB]",
            &northern_lights("book", None)
        ),
        ScoringDecision::Candidate
    );
}

#[test]
fn another_book_by_the_author_is_still_not_the_book() {
    let want = northern_lights("book", Some(vec!["The Golden Compass"]));
    for title in [
        "Philip Pullman - The Subtle Knife (1997) [EPUB]",
        "Philip Pullman - The Amber Spyglass (2000) [EPUB]",
    ] {
        assert_ne!(
            decision(title, &want),
            ScoringDecision::Candidate,
            "{title}"
        );
    }
}
