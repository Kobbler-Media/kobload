//! Reusing match-gate answers across a source's consecutive fetches must not
//! change what the RSS path selects or queues for review.
//!
//! The corpus test replays consecutive captured fetches of each source, with
//! the feed overlap a capture drops put back, with and without reuse. It is
//! skipped unless `KOBLOAD_SCORING_CORPUS` is set:
//!
//!   KOBLOAD_SCORING_CORPUS=/path/to/capture \
//!     cargo test --release --test integration rss_gate_memo_test:: -- --nocapture

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use kobload_api::watch::capture::{overlapping_feeds, read_capture};
use kobload_api::watch::gate_memo::MatchGateMemo;
use kobload_api::watch::{PreparedPattern, Release, ScoringPassStats, rss_outcome};
use rayon::prelude::*;
use serde_json::{Value, json};
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

const PATTERN_ID: Uuid = Uuid::from_u128(0x0199_0000_0000_7000_8000_0000_0000_00a1);
const SOURCE_ID: Uuid = Uuid::from_u128(0x0199_0000_0000_7000_8000_0000_0000_00a2);

fn at(minutes: i64) -> OffsetDateTime {
    OffsetDateTime::parse("2026-10-12T12:00:00Z", &Rfc3339).unwrap()
        + time::Duration::minutes(minutes)
}

fn release(guid: &str, title: &str) -> Arc<Release> {
    serde_json::from_value(json!({
        "guid": guid,
        "title": title,
        "size": 2_000_000_000i64,
        "publish_date": "2026-10-12T12:00:00Z",
        "category_ids": [],
        "indexer": "test",
        "protocol": "torrent",
        "seeders": 10,
        "raw_data": {},
    }))
    .unwrap()
}

/// PreparedPattern's fields are private to the crate; build it the way a
/// capture file stores it.
fn patterns(target: &str, matched_guids: &[&str]) -> Vec<PreparedPattern> {
    let now = serde_json::to_value(at(0)).unwrap();
    serde_json::from_value(json!([{
        "pattern": {
            "id": PATTERN_ID,
            "source_system": "test",
            "source_id": "svu",
            "callback_url": "http://localhost/callback",
            "patterns": [],
            "media_want": null,
            "match_targets": null,
            "match_mode": null,
            "min_score": 0.75,
            "media_type_id": null,
            "auto_download": true,
            "metadata": null,
            "enabled": true,
            "upgrade_allowed": true,
            "upgrade_until": null,
            "created_at": now,
            "updated_at": now,
            "last_backlog_search_at": null,
            "backlog_search_interval_hours": null,
            "excluded_releases": null,
            "search_not_before": null,
        },
        "quality_profile": null,
        "match_config": {
            "patterns": [],
            "match_targets": [{"id": "t1", "target": target, "min_score": null}],
            "match_mode": "fuzzy",
            "min_score": 0.75
        },
        "categories": [],
        "matched_guids": matched_guids,
        "_is_season": false,
        "parsed_media_want": null,
        "episodes_needed": null,
        "preferred_protocol": null
    }]))
    .unwrap()
}

/// Score `feed` with and without reuse, assert both decide the same, and
/// return the pass's counters and the decision.
fn same_with_and_without_reuse(
    memo: &mut MatchGateMemo,
    prepared: &[PreparedPattern],
    feed: &[Arc<Release>],
    source_id: Uuid,
    trash: Option<&kobload_api::trash_filters::TrashFilterSet>,
    captured_at: OffsetDateTime,
    reviews: bool,
) -> (ScoringPassStats, Value) {
    let (plain, plain_stats) =
        rss_outcome(prepared, feed, source_id, trash, captured_at, reviews, None);
    let (reused, stats) = rss_outcome(
        prepared,
        feed,
        source_id,
        trash,
        captured_at,
        reviews,
        Some(memo),
    );
    assert_eq!(
        reused, plain,
        "reusing gates changed the outcome at {captured_at}"
    );
    assert_eq!(
        (stats.pairs, stats.fully_scored),
        (plain_stats.pairs, plain_stats.fully_scored)
    );
    (stats, serde_json::to_value(&plain).unwrap())
}

#[test]
fn reused_gates_decide_like_fresh_ones() {
    let hit = release("a", "Law.and.Order.SVU.S28E02.1080p.WEB.H264-FLUX");
    let miss = release("b", "Some.Other.Show.S01E01.1080p.WEB.H264-GRP");
    let late = release("c", "Law.and.Order.SVU.S28E02.720p.HDTV.x264-SYNCOPY");
    let e02 = patterns("Law and Order SVU S28E02", &[]);

    for reviews in [false, true] {
        let mut memo = MatchGateMemo::default();
        let mut step = |prepared: &[PreparedPattern], feed: &[Arc<Release>], minutes| {
            let (stats, outcome) = same_with_and_without_reuse(
                &mut memo,
                prepared,
                feed,
                SOURCE_ID,
                None,
                at(minutes),
                reviews,
            );
            (stats.gates_reused, outcome)
        };
        let candidate_guid = |outcome: &Value| outcome["candidates"][0]["guid"].clone();

        let (reused, outcome) = step(&e02, &[hit.clone(), miss.clone()], 0);
        assert_eq!((reused, candidate_guid(&outcome)), (0, json!("a")));

        // The next poll repeats both and adds one: only the new one is asked.
        let feed = [late.clone(), hit.clone(), miss.clone()];
        let (reused, outcome) = step(&e02, &feed, 15);
        assert_eq!((reused, candidate_guid(&outcome)), (2, json!("a")));

        // A changed target invalidates every answer.
        let other = patterns("Some Other Show S01E01", &[]);
        let (reused, outcome) = step(&other, &feed, 30);
        assert_eq!((reused, candidate_guid(&outcome)), (0, json!("b")));

        // Back to the old config: answers from two fetches ago are gone, and a
        // release dropped by matched GUIDs is never asked, so never recorded.
        let (reused, outcome) = step(&patterns("Law and Order SVU S28E02", &["a"]), &feed, 45);
        assert_eq!((reused, candidate_guid(&outcome)), (0, json!("c")));

        // Once it's no longer matched, the gate has to run for it again,
        // hours later, when freshness has moved on.
        let (reused, outcome) = step(&e02, &feed, 600);
        assert_eq!((reused, candidate_guid(&outcome)), (2, json!("a")));
    }
}

#[test]
fn reused_gates_decide_like_fresh_ones_on_the_corpus() {
    let Some(dir) = std::env::var_os("KOBLOAD_SCORING_CORPUS").map(PathBuf::from) else {
        eprintln!("KOBLOAD_SCORING_CORPUS not set; skipping gate reuse replay");
        return;
    };
    let corpus = read_capture(&dir).expect("read capture directory");
    let feeds = overlapping_feeds(&corpus, 100);

    let mut by_source: BTreeMap<Uuid, Vec<usize>> = BTreeMap::new();
    for (i, f) in corpus.iter().enumerate() {
        by_source.entry(f.fetch.source_id).or_default().push(i);
    }
    let per_source: Vec<(usize, usize, usize)> = by_source
        .into_par_iter()
        .map(|(source_id, fetches)| {
            let mut memo = MatchGateMemo::default();
            let (mut pairs, mut reused) = (0, 0);
            for &i in &fetches {
                let f = &corpus[i];
                let (stats, _) = same_with_and_without_reuse(
                    &mut memo,
                    &f.prepared,
                    &feeds[i],
                    source_id,
                    f.trash.as_deref(),
                    f.fetch.captured_at,
                    true,
                );
                pairs += stats.pairs;
                reused += stats.gates_reused;
            }
            (fetches.len(), pairs, reused)
        })
        .collect();
    let fetches: usize = per_source.iter().map(|s| s.0).sum();
    let pairs: usize = per_source.iter().map(|s| s.1).sum();
    let reused: usize = per_source.iter().map(|s| s.2).sum();
    eprintln!(
        "{fetches} overlapping fetches decided the same: {pairs} pairs, {reused} gates reused"
    );
    assert!(reused > 0, "no gate was ever reused");
}
