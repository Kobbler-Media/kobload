//! Every path that scores a TV episode want must apply the XEM scene-numbering
//! bonus.
//!
//! The want is TVDB S01E13, which the scene numbers S02E01. Scene S01E13 is a
//! different episode (TVDB S01E14), so a release labelled S01E13 is a
//! numbering collision and XEM withholds its +0.5 episode bonus. Without XEM
//! the collision keeps the bonus and outscores an unambiguous 2160p release
//! numbered 13 absolute, and the 720p collision is picked.

use std::sync::Arc;

use serde_json::{Value, json};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::common::stream_fixtures::{create_provider, hash};
use crate::common::tv_watch::{TvWatch, episode_want, usenet_release};
use kobload_api::decision_log::repository::{get_session_detail, list_sessions};
use kobload_api::jobs::handlers::{SearchGates, backlog_search_pattern};
use kobload_api::repositories::hash_index::{self, HashIndexEntry};
use kobload_api::repositories::{app_settings, watch as watch_repo, xem_cache};
use kobload_api::services::stream::{StreamSearchContext, stream_search};
use kobload_api::watch::holds::process_expired_holds;
use kobload_api::watch::{PreparedPattern, Release, process_releases, rss_outcome};
use nats_jobs::types::StreamSearchRequest;

const TVDB_ID: i64 = 424_242;
const ABSOLUTE_GUID: &str = "xem-absolute-13";
const COLLISION_GUID: &str = "xem-collision-s01e13";

fn show_map() -> Value {
    let entry = |scene: (i32, i32), tvdb: (i32, i32)| {
        json!({
            "scene": {"season": scene.0, "episode": scene.1, "absolute": null},
            "tvdb": {"season": tvdb.0, "episode": tvdb.1, "absolute": null},
        })
    };
    json!({"entries": [entry((2, 1), (1, 13)), entry((1, 13), (1, 14))]})
}

fn media_want() -> nats_jobs::MediaWant {
    episode_want("Kitsune Days", "1", "13", Some(TVDB_ID))
}

fn releases() -> Vec<Release> {
    vec![
        usenet_release(COLLISION_GUID, "Kitsune.Days.S01E13.720p.HDTV.x264-GRP"),
        usenet_release(
            ABSOLUTE_GUID,
            "[Erai-raws] Kitsune Days - 13 [2160p][Multiple Subtitle]",
        ),
    ]
}

async fn start() -> (TvWatch, watch_repo::WatchPatternRow) {
    let tv = TvWatch::start().await;
    tv.indexer.set_search_results(releases());
    xem_cache::put(&tv.server.pool, TVDB_ID, &show_map())
        .await
        .unwrap();
    let pattern = tv.pattern(&media_want()).await;
    (tv, pattern)
}

#[tokio::test]
async fn rss_poll_skips_the_numbering_collision() {
    let (tv, pattern) = start().await;

    process_releases(
        &tv.server.app_state,
        &tv.source,
        &releases(),
        std::slice::from_ref(&pattern),
    )
    .await;

    assert_eq!(tv.grabbed(pattern.id).await, [ABSOLUTE_GUID]);
    tv.server.cleanup().await;
}

#[tokio::test]
async fn backlog_search_skips_the_numbering_collision() {
    let (tv, pattern) = start().await;

    backlog_search_pattern(&tv.server.app_state, &pattern, SearchGates::AUTOMATIC).await;

    assert_eq!(tv.grabbed(pattern.id).await, [ABSOLUTE_GUID]);
    tv.server.cleanup().await;
}

#[tokio::test]
async fn backlog_search_logs_the_quality_breakdown_it_scored() {
    let (tv, pattern) = start().await;
    app_settings::set(
        &tv.server.pool,
        app_settings::keys::SEARCH_LOGGING_ENABLED,
        &json!(true),
    )
    .await
    .unwrap();

    backlog_search_pattern(&tv.server.app_state, &pattern, SearchGates::AUTOMATIC).await;

    // The session is flushed on a spawned task.
    let mut session = None;
    for _ in 0..50 {
        session = list_sessions(&tv.server.pool, None, None, Some(pattern.id), 1, 0)
            .await
            .unwrap()
            .pop();
        if session.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let detail = get_session_detail(&tv.server.pool, session.expect("backlog session").id)
        .await
        .unwrap()
        .unwrap();
    let grabbed = detail
        .releases
        .iter()
        .find(|r| r.release_guid.as_deref() == Some(ABSOLUTE_GUID))
        .expect("grabbed release logged");
    assert!(grabbed.quality_score_breakdown.is_some());
    tv.server.cleanup().await;
}

#[tokio::test]
async fn final_search_at_hold_expiry_skips_the_numbering_collision() {
    let (tv, pattern) = start().await;
    watch_repo::create_match(
        &tv.server.pool,
        &watch_repo::WatchMatchInput {
            pattern_id: pattern.id,
            indexer_source_id: Some(tv.source.id),
            release_guid: "xem-held-torrent",
            release_title: "Kitsune.Days.S01E13.1080p.WEB.x264-TOR",
            release_data: Some(&json!({})),
            match_score: 0.9,
            matched_pattern: "Kitsune Days S01E13",
            action: "held",
            download_id: None,
            hold_until: Some(OffsetDateTime::now_utc() - time::Duration::minutes(1)),
            held: true,
            quality_score: Some(0.3),
            rejected: false,
            rejected_at: None,
            rejected_reason: None,
            episode_key: None,
        },
    )
    .await
    .unwrap();

    process_expired_holds(&tv.server.app_state).await;

    assert_eq!(tv.grabbed(pattern.id).await, [ABSOLUTE_GUID]);
    tv.server.cleanup().await;
}

#[tokio::test]
async fn search_routes_rank_the_unambiguous_release_first() {
    let (tv, _) = start().await;
    let mw = serde_json::to_value(media_want()).unwrap();
    let cases = [
        (
            "/api/v1/search-and-download",
            json!({"patterns": [], "media_want": mw, "media_type": "tv", "min_score": 0.5}),
            "results",
        ),
        (
            "/api/v1/providers/releases/search",
            json!({"media_want": mw, "min_score": 0.5}),
            "candidates",
        ),
    ];

    let mut rankings = Vec::new();
    for (path, request, ranked) in cases {
        let response = tv.server.admin_client().post_json(path, &request).await;
        response.assert_ok();
        let body: Value = response.json();
        let order: Vec<String> = body[ranked]
            .as_array()
            .unwrap()
            .iter()
            .map(|r| r["guid"].as_str().unwrap().to_owned())
            .collect();
        rankings.push((path, order));
    }
    let expected: Vec<_> = rankings
        .iter()
        .map(|(path, _)| {
            (
                *path,
                [ABSOLUTE_GUID, COLLISION_GUID].map(String::from).to_vec(),
            )
        })
        .collect();
    assert_eq!(rankings, expected);
    tv.server.cleanup().await;
}

// Spaced, because the hash index finds titles by full-text search and a
// dotted title is one token.
const STREAM_COLLISION: &str = "Kitsune Days S01E13 720p HDTV x264-GRP";
const STREAM_ABSOLUTE: &str = "[Erai-raws] Kitsune Days - 13 [2160p][Multiple Subtitle]";

fn torrent(guid: &str, title: &str, hash_char: char) -> Release {
    let mut release = usenet_release(guid, title);
    let info_hash = hash(hash_char);
    release.protocol = "torrent".to_string();
    release.download_url = None;
    release.magnet_url = Some(format!("magnet:?xt=urn:btih:{info_hash}"));
    release.info_hash = Some(info_hash);
    release
}

fn hash_index_entry(release: &Release) -> HashIndexEntry {
    HashIndexEntry {
        info_hash: release.info_hash.clone().unwrap(),
        imdb_id: None,
        tmdb_id: None,
        original_title: release.title.clone(),
        parsed_title: None,
        year: None,
        quality: None,
        size: Some(release.size),
        magnet_url: release.magnet_url.clone(),
        source: "test".to_string(),
        seeders: Some(10),
        season: None,
        episodes: None,
    }
}

#[derive(Clone, Copy, Debug)]
enum StreamSource {
    HashIndex,
    IndexerFallback,
}

#[tokio::test]
async fn stream_search_ranks_the_unambiguous_release_first() {
    let mut rankings = Vec::new();
    for source in [StreamSource::HashIndex, StreamSource::IndexerFallback] {
        let (tv, _) = start().await;
        let pool = &tv.server.pool;
        let torrents = [
            torrent(COLLISION_GUID, STREAM_COLLISION, 'c'),
            torrent(ABSOLUTE_GUID, STREAM_ABSOLUTE, 'a'),
        ];
        match source {
            StreamSource::HashIndex => {
                tv.indexer.set_search_results(Vec::new());
                let entries: Vec<_> = torrents.iter().map(hash_index_entry).collect();
                hash_index::upsert_batch(pool, &entries).await.unwrap();
            }
            StreamSource::IndexerFallback => tv.indexer.set_search_results(torrents.to_vec()),
        }
        create_provider(pool, "real_debrid").await;
        sqlx::query("UPDATE indexer_sources SET debrid_mode = 'both' WHERE id = $1")
            .bind(tv.source.id)
            .execute(pool)
            .await
            .unwrap();

        let state = &tv.server.app_state;
        let unreachable = reqwest::Client::builder()
            .proxy(reqwest::Proxy::all("http://127.0.0.1:9").unwrap())
            .build()
            .unwrap();
        let ctx = StreamSearchContext {
            pool,
            http_client: &unreachable,
            indexers: state.indexers.as_ref(),
            debrid_providers: state.debrid_providers.as_ref(),
            definitions: &state.definitions,
            xem_cache: state.xem_cache.as_ref(),
            scene_mappings: state.scene_mappings.as_ref(),
            trash_filters: state.trash_filters.as_ref(),
            download_events: &state.download_events,
            debrid_providers_arc: state.debrid_providers.clone(),
        };
        let request = StreamSearchRequest {
            media_want: Some(media_want()),
            query: String::new(),
            media_type: Some("tv".to_string()),
            quality_profile_id: None,
            limit: None,
            include_uncached: Some(true),
            prefer: None,
            audio_languages: Vec::new(),
        };
        let response = stream_search(&ctx, &request).await.unwrap();
        let order: Vec<String> = response
            .options
            .iter()
            .map(|o| o.release_title.clone())
            .collect();
        rankings.push((format!("{source:?}"), order));
        tv.server.cleanup().await;
    }

    let expected: Vec<_> = rankings
        .iter()
        .map(|(source, _)| {
            (
                source.clone(),
                [STREAM_ABSOLUTE, STREAM_COLLISION]
                    .map(String::from)
                    .to_vec(),
            )
        })
        .collect();
    assert_eq!(rankings, expected);
}

#[test]
fn captured_patterns_replay_with_the_xem_bonus() {
    let now = serde_json::to_value(OffsetDateTime::now_utc()).unwrap();
    let mw = serde_json::to_value(media_want()).unwrap();
    let target = |id: &str, target: &str| json!({"id": id, "target": target, "min_score": null});
    let prepared: Vec<PreparedPattern> = serde_json::from_value(json!([{
        "pattern": {
            "id": Uuid::nil(),
            "source_system": "test",
            "source_id": "kitsune-days-s01e13",
            "callback_url": "http://localhost/callback",
            "patterns": [],
            "media_want": mw,
            "match_targets": null,
            "match_mode": null,
            "min_score": 0.5,
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
            "patterns": ["Kitsune Days S01E13"],
            "match_targets": [
                target("tvdb", "Kitsune Days S01E13"),
                target("scene", "Kitsune Days S02E01"),
            ],
            "match_mode": "fuzzy",
            "min_score": 0.5
        },
        "categories": [],
        "matched_guids": [],
        "_is_season": false,
        "parsed_media_want": mw,
        "episodes_needed": null,
        "preferred_protocol": null,
        "xem_show_map": show_map(),
    }]))
    .unwrap();
    let releases: Vec<Arc<Release>> = releases().into_iter().map(Arc::new).collect();

    let (outcome, _) = rss_outcome(
        &prepared,
        &releases,
        Uuid::nil(),
        None,
        OffsetDateTime::now_utc(),
        false,
        None,
    );
    let picked: Vec<&str> = outcome.candidates.iter().map(|c| c.guid.as_str()).collect();
    assert_eq!(picked, [ABSOLUTE_GUID]);
}
