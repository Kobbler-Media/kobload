use std::sync::Arc;

use kobload_api::decision_log::repository::{flush_session, get_session_detail};
use kobload_api::decision_log::{DecisionCollector, LogRelease};
use kobload_api::watch::{PreparedPattern, Release, rss_feed_log};
use serde_json::{Value, json};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::common::TestServer;

const PICKED: &str = "MotoGP.2026.Americas.Race.Mystery.1080p.WEB.h264-GRP";
const WRONG_SESSION: &str = "MotoGP.2026.Americas.Qualifying.1080p.WEB.h264-GRP";

fn release(title: &str) -> Arc<Release> {
    serde_json::from_value(json!({
        "guid": title,
        "title": title,
        "size": 2_000_000_000i64,
        "publish_date": "2026-03-29T21:00:00Z",
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
    let want = json!({
        "kind": "sport_event", "league_name": "MotoGP", "season": "2026", "round": 3,
        "location": "USA", "session_type": "Race", "event_date": "2026-03-29",
        "quality_profile_id": null, "quality_overrides": null,
        "venue": "Circuit of the Americas", "city": "Austin", "region": "Texas",
        "country": "United States", "country_iso": "US", "class": "MotoGP",
        "location_aliases": ["Circuit of the Americas", "COTA", "Americas", "Austin"],
    });
    serde_json::from_value(json!([{
        "pattern": {
            "id": Uuid::nil(), "source_system": "test", "source_id": "motogp-americas-race",
            "callback_url": "http://localhost/callback", "patterns": [], "media_want": want,
            "match_targets": null, "match_mode": null, "min_score": 0.5, "media_type_id": null,
            "auto_download": true, "metadata": null, "enabled": true, "upgrade_allowed": true,
            "upgrade_until": null, "created_at": now, "updated_at": now,
            "last_backlog_search_at": null, "backlog_search_interval_hours": null,
            "excluded_releases": null, "search_not_before": null,
        },
        "quality_profile": null,
        "match_config": {
            "patterns": [],
            "match_targets": [{"id": "sport", "target": "MotoGP 2026 USA Race", "min_score": null}],
            "match_mode": "fuzzy",
            "min_score": 0.5
        },
        "categories": [], "matched_guids": [], "_is_season": false,
        "parsed_media_want": want, "episodes_needed": null, "preferred_protocol": null
    }]))
    .unwrap()
}

fn feed_log() -> Vec<LogRelease> {
    rss_feed_log(
        &prepared(),
        &[release(PICKED), release(WRONG_SESSION)],
        Uuid::nil(),
        None,
        OffsetDateTime::now_utc(),
    )
}

fn picked_explanation() -> Value {
    json!({
        "matched": [
            {"slot": "class", "value": "motogp", "alias": "motogp"},
            {"slot": "season", "value": "2026"},
            {"slot": "session", "value": "race", "alias": "race"},
            {"slot": "place", "value": "americas", "alias": "Americas"},
        ],
        "contradictions": [],
        "unexplained": ["mystery"],
    })
}

fn wrong_session_explanation() -> Value {
    json!({
        "matched": [
            {"slot": "class", "value": "motogp", "alias": "motogp"},
            {"slot": "season", "value": "2026"},
            {"slot": "place", "value": "americas", "alias": "Americas"},
        ],
        "contradictions": [{"slot": "session", "value": "qualifying"}],
        "unexplained": [],
    })
}

#[test]
fn feed_log_explains_each_sport_decision() {
    let rows: Vec<(String, String, Value)> = feed_log()
        .into_iter()
        .map(|r| {
            (
                r.release_title,
                r.decision,
                serde_json::to_value(r.match_explanation).unwrap(),
            )
        })
        .collect();

    assert_eq!(
        rows,
        [
            (PICKED.to_string(), "best".to_string(), picked_explanation()),
            (
                WRONG_SESSION.to_string(),
                "rejected".to_string(),
                wrong_session_explanation()
            ),
        ]
    );
}

#[tokio::test]
async fn decision_log_stores_the_explanation() {
    let server = TestServer::start().await;
    let collector = DecisionCollector::new("feed_sync", None, None, true);
    let session_id = collector.session_id();
    for row in feed_log() {
        collector.record_release(LogRelease {
            provider_call_id: None,
            ..row
        });
    }

    flush_session(&server.pool, collector.into_flush_payload())
        .await
        .unwrap();

    let stored: Vec<(String, Option<Value>)> = get_session_detail(&server.pool, session_id)
        .await
        .unwrap()
        .unwrap()
        .releases
        .into_iter()
        .map(|r| (r.release_title, r.match_explanation))
        .collect();
    assert_eq!(
        stored,
        [
            (PICKED.to_string(), Some(picked_explanation())),
            (WRONG_SESSION.to_string(), Some(wrong_session_explanation())),
        ]
    );
    server.cleanup().await;
}
