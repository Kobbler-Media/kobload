//! Scoring capture and replay: the harness that later scoring optimisations are
//! checked against. Uses synthetic data; the real corpus is exercised by
//! `scoring_replay_test.rs`.

use std::path::Path;
use std::sync::Arc;

use kobload_api::clock;
use kobload_api::repositories::trash_filters::TrashFilterRow;
use kobload_api::repositories::watch::WatchPatternRow;
use kobload_api::trash_filters::TrashFilterSet;
use kobload_api::watch::capture::{ScoringCapture, read_capture};
use kobload_api::watch::{PreparedPattern, Release, replay_scoring, rss_scoring_pass};
use serde_json::json;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;
use uuid::Uuid;

const PATTERN_ID: Uuid = Uuid::from_u128(0x0199_0000_0000_7000_8000_0000_0000_0001);
const SOURCE_ID: Uuid = Uuid::from_u128(0x0199_0000_0000_7000_8000_0000_0000_0002);

fn captured_at() -> OffsetDateTime {
    OffsetDateTime::parse("2026-10-12T12:00:00Z", &Rfc3339).unwrap()
}

fn release(guid: &str, title: &str) -> Release {
    Release {
        guid: guid.to_string(),
        title: title.to_string(),
        file_name: None,
        size: 2_000_000_000,
        indexer: "test".to_string(),
        protocol: "torrent".to_string(),
        download_url: None,
        magnet_url: None,
        info_url: None,
        publish_date: "2026-10-12T12:00:00Z".to_string(),
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
        raw_data: json!({"attr": "value"}),
        external_ids: Vec::new(),
        media: None,
    }
}

fn prepared_patterns() -> Vec<PreparedPattern> {
    let now = captured_at();
    let row = WatchPatternRow {
        id: PATTERN_ID,
        source_system: "test".to_string(),
        source_id: "svu-s28e02".to_string(),
        callback_url: "http://localhost/callback".to_string(),
        patterns: Vec::new(),
        media_want: None,
        match_targets: None,
        match_mode: None,
        min_score: Some(0.75),
        media_type_id: None,
        auto_download: true,
        metadata: None,
        enabled: true,
        upgrade_allowed: true,
        upgrade_until: None,
        created_at: now,
        updated_at: now,
        last_backlog_search_at: None,
        backlog_search_interval_hours: None,
        excluded_releases: None,
        search_not_before: None,
    };
    // PreparedPattern's fields are private to the crate; build it the way a
    // capture file stores it.
    serde_json::from_value(json!([{
        "pattern": serde_json::to_value(&row).unwrap(),
        "quality_profile": null,
        "match_config": {
            "patterns": [],
            "match_targets": [{
                "id": "t1",
                "target": "Law and Order SVU S28E02",
                "min_score": null
            }],
            "match_mode": "fuzzy",
            "min_score": 0.75
        },
        "categories": [],
        "matched_guids": [],
        "_is_season": false,
        "parsed_media_want": null,
        "episodes_needed": null,
        "preferred_protocol": null
    }]))
    .unwrap()
}

fn open(dir: &Path, max_bytes: u64) -> ScoringCapture {
    ScoringCapture::open(dir, time::Duration::hours(1), max_bytes).unwrap()
}

#[test]
fn capture_round_trips_and_replays() {
    let dir = tempfile::tempdir().unwrap();
    let prepared = prepared_patterns();
    let hit = release("g-hit", "Law.and.Order.SVU.S28E02.1080p.WEB.H264-FLUX");
    let miss = release("g-miss", "Some.Other.Show.S01E01.1080p.WEB.H264-GRP");

    // Capture windows run from real time, so record "now".
    let now = OffsetDateTime::now_utc();
    let mut capture = open(dir.path(), 10 * 1024 * 1024);
    capture.record_fetch(
        now,
        SOURCE_ID,
        "src",
        "newznab",
        &prepared,
        None,
        &[hit.clone(), miss.clone()],
    );
    // A repeat poll returning the same items adds nothing.
    capture.record_fetch(
        now,
        SOURCE_ID,
        "src",
        "newznab",
        &prepared,
        None,
        &[hit.clone(), miss],
    );
    let late = release("g-late", "Law.and.Order.SVU.S28E02.720p.HDTV.x264-SYNCOPY");
    capture.record_fetch(
        now,
        SOURCE_ID,
        "src",
        "newznab",
        &prepared,
        None,
        &[hit, late],
    );
    assert!(capture.is_active());
    assert!(capture.used_bytes() > 0);

    let corpus = read_capture(dir.path()).unwrap();
    assert_eq!(corpus.len(), 2, "duplicate fetch should not be recorded");
    let guids: Vec<_> = corpus[0].fetch.releases.iter().map(|r| r.guid.as_str()).collect();
    assert_eq!(guids, ["g-hit", "g-miss"]);
    let guids: Vec<_> = corpus[1].fetch.releases.iter().map(|r| r.guid.as_str()).collect();
    assert_eq!(guids, ["g-late"]);
    assert!(
        Arc::ptr_eq(&corpus[0].prepared, &corpus[1].prepared),
        "an unchanged pattern set is stored once"
    );
    assert_eq!(corpus[0].fetch.releases[0].raw_data, json!({"attr": "value"}));

    let first = &corpus[0];
    let replay = replay_scoring(
        &first.prepared,
        &first.fetch.releases,
        first.fetch.source_id,
        first.trash.as_deref(),
        captured_at(),
    );
    assert_eq!(replay.matched.len(), 1);
    assert_eq!(replay.matched[0]["release"], "g-hit");
    assert_eq!(replay.matched[0]["pattern_id"], PATTERN_ID.to_string());
    assert_eq!(replay.candidates.len(), 1);
    assert_eq!(replay.candidates[0].guid, "g-hit");
    assert_eq!(replay.candidates[0].pattern_id, PATTERN_ID);
    assert_eq!(replay.stats.pairs, 2);
    assert_eq!(replay.stats.fully_scored, 1, "the no-match is skipped");
    assert!(replay.review.contains_key(&PATTERN_ID));

    let stats = rss_scoring_pass(
        &first.prepared,
        &first.fetch.releases,
        first.fetch.source_id,
        None,
        captured_at(),
    );
    assert_eq!((stats.pairs, stats.fully_scored), (2, 1));
}

#[test]
fn replay_is_pinned_to_capture_time() {
    let prepared = prepared_patterns();
    let releases = [release("g-hit", "Law.and.Order.SVU.S28E02.1080p.WEB.H264-FLUX")];
    let replay = replay_scoring(&prepared, &releases, SOURCE_ID, None, captured_at());
    // Published at the capture time, so zero days old: the full freshness
    // bonus, whatever today's date is.
    assert_eq!(replay.matched[0]["freshness_bonus"], json!(0.08));

    let again = replay_scoring(&prepared, &releases, SOURCE_ID, None, captured_at());
    assert_eq!(
        serde_json::to_value(&replay).unwrap(),
        serde_json::to_value(&again).unwrap()
    );
}

#[test]
fn capture_stops_at_size_limit() {
    let dir = tempfile::tempdir().unwrap();
    // capture.json alone exceeds one byte.
    let mut capture = open(dir.path(), 1);
    capture.record_fetch(
        OffsetDateTime::now_utc(),
        SOURCE_ID,
        "src",
        "newznab",
        &prepared_patterns(),
        None,
        &[release("g1", "Law.and.Order.SVU.S28E02.1080p.WEB")],
    );
    assert!(!capture.is_active());
    assert!(!dir.path().join("fetches.jsonl.gz").exists());
}

#[test]
fn capture_stops_after_window_and_restart_keeps_it() {
    let dir = tempfile::tempdir().unwrap();
    let capture = open(dir.path(), 10 * 1024 * 1024);
    drop(capture);

    // Reopening with a longer window must not extend the original one.
    let mut capture =
        ScoringCapture::open(dir.path(), time::Duration::days(30), 10 * 1024 * 1024).unwrap();
    capture.record_fetch(
        OffsetDateTime::now_utc() + time::Duration::hours(2),
        SOURCE_ID,
        "src",
        "newznab",
        &prepared_patterns(),
        None,
        &[release("g1", "Law.and.Order.SVU.S28E02.1080p.WEB")],
    );
    assert!(!capture.is_active());
    assert!(!dir.path().join("fetches.jsonl.gz").exists());
}

#[test]
fn trash_rows_round_trip_through_capture() {
    let dir = tempfile::tempdir().unwrap();
    let rows = vec![TrashFilterRow {
        category: "lq".to_string(),
        spec_name: "Sample".to_string(),
        filter_type: "release_title".to_string(),
        pattern: "(?i)\\bsample\\b".to_string(),
        negate: false,
        required: false,
        tier_bonus: None,
    }];
    let mut capture = open(dir.path(), 10 * 1024 * 1024);
    capture.record_fetch(
        OffsetDateTime::now_utc(),
        SOURCE_ID,
        "src",
        "newznab",
        &prepared_patterns(),
        Some(&rows),
        &[release("g1", "Law.and.Order.SVU.S28E02.1080p.WEB")],
    );
    let corpus = read_capture(dir.path()).unwrap();
    let trash = corpus[0].trash.as_ref().expect("trash rows were captured");
    assert_eq!(trash.check_blocklist("Show.S01E01.SAMPLE.mkv", None), Some("lq"));
    assert_eq!(trash.check_blocklist("Show.S01E01.1080p.mkv", None), None);
}

#[test]
fn trash_blocklist_reports_categories_in_a_stable_order() {
    let row = |category: &str| TrashFilterRow {
        category: category.to_string(),
        spec_name: "Sample".to_string(),
        filter_type: "release_title".to_string(),
        pattern: "(?i)sample".to_string(),
        negate: false,
        required: false,
        tier_bonus: None,
    };
    // Both categories match; the reported one must not depend on hash order.
    for _ in 0..20 {
        let set = TrashFilterSet::from_rows(&[row("zz_last"), row("aa_first"), row("mm_mid")]);
        assert_eq!(set.check_blocklist("Show.SAMPLE", None), Some("aa_first"));
    }
}

#[test]
fn pinned_clock_is_restored() {
    let pinned = captured_at();
    let inside = clock::with_pinned_now(pinned, clock::now_utc);
    assert_eq!(inside, pinned);
    assert_ne!(clock::now_utc(), pinned);
}
