//! What the RSS feed log (decision log with feed logging on) keeps for a
//! fetch: every release that matched a pattern, picked or rejected, with its
//! reasons, the winner marked best, and nothing for releases that matched no
//! pattern.

use std::sync::Arc;

use kobload_api::watch::{PreparedPattern, Release, rss_feed_log};
use serde_json::json;
use time::OffsetDateTime;
use uuid::Uuid;

const PICKED: &str = "Law.and.Order.SVU.S28E02.1080p.WEB.H264-FLUX";
const REJECTED: &str = "Law.and.Order.SVU.S28E02.480p.HDTV.x264-GRP";
const UNRELATED: &str = "Some.Other.Show.S01E01.480p.HDTV.x264-GRP";

fn release(title: &str) -> Arc<Release> {
    serde_json::from_value(json!({
        "guid": title,
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

fn prepared() -> Vec<PreparedPattern> {
    let now = serde_json::to_value(OffsetDateTime::now_utc()).unwrap();
    serde_json::from_value(json!([{
        "pattern": {
            "id": Uuid::nil(),
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
        "quality_profile": {
            "id": Uuid::nil(), "name": "tv", "media_kind": "tv",
            "min_resolution": "720p", "max_resolution": null,
            "preferred_sources": [], "preferred_codecs": [], "preferred_audio": [],
            "preferred_languages": [], "rejected_sources": [], "rejected_languages": [],
            "rejected_terms": [], "reject_hardcoded_subs": false, "reject_remux": false,
            "use_trash_filters": true, "is_default": true, "upgrade_cutoff_score": null,
            "created_at": now, "updated_at": now,
        },
        "match_config": {
            "patterns": [],
            "match_targets": [{"id": "t1", "target": "Law and Order SVU S28E02", "min_score": null}],
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

#[test]
fn feed_log_keeps_what_matched_and_why() {
    let releases = [release(PICKED), release(REJECTED), release(UNRELATED)];
    let log = rss_feed_log(
        &prepared(),
        &releases,
        Uuid::nil(),
        None,
        OffsetDateTime::now_utc(),
    );

    let rows: Vec<(&str, &str, Option<&str>, bool)> = log
        .iter()
        .map(|r| {
            (
                r.release_title.as_str(),
                r.decision.as_str(),
                r.decision_reason.as_deref(),
                r.is_best,
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            (PICKED, "best", None, true),
            (REJECTED, "rejected", Some("below_min_resolution"), false),
        ]
    );
    for row in &log {
        assert!(row.match_score.is_some(), "{}", row.release_title);
        assert!(
            row.quality_score_breakdown.is_some(),
            "{}",
            row.release_title
        );
    }
}
