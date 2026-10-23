//! Every TV search entry point must match a release by its XEM scene numbering
//! and by the want's alternate titles.
//!
//! The want is TVDB S01E13, which the scene numbers S02E01. Scene S02E02 is
//! the next episode and must still be rejected.

use std::collections::BTreeSet;

use serde_json::{Value, json};
use time::OffsetDateTime;

use crate::common::stream_fixtures::hash;
use crate::common::tv_watch::{TvWatch, episode_want, usenet_release};
use kobload_api::decision_log::repository::get_session_detail;
use kobload_api::jobs::handlers::{SearchGates, backlog_search_pattern};
use kobload_api::repositories::{watch as watch_repo, xem_cache};
use kobload_api::watch::holds::process_expired_holds;
use kobload_api::watch::{Release, process_releases};

const TVDB_ID: i64 = 515_151;
const SCENE_NUMBERED: &str = "Kitsune.Days.S02E01.1080p.WEB-DL.x264-GRP";
const ALTERNATE_TITLE: &str = "Renard.Days.S01E13.1080p.WEB-DL.x264-GRP";
const NEXT_SCENE_EPISODE: &str = "Kitsune.Days.S02E02.1080p.WEB-DL.x264-GRP";
const HELD_GUID: &str = "held-before-the-final-search";

fn media_want() -> nats_jobs::MediaWant {
    let mut want = episode_want("Kitsune Days", "1", "13", Some(TVDB_ID));
    if let nats_jobs::MediaWant::TvEpisode {
        alternate_titles, ..
    } = &mut want
    {
        *alternate_titles = Some(vec!["Renard Days".to_string()]);
    }
    want
}

fn torrent(title: &str, hash_char: char) -> Release {
    let mut release = usenet_release(title, title);
    let info_hash = hash(hash_char);
    release.protocol = "torrent".to_string();
    release.download_url = None;
    release.magnet_url = Some(format!("magnet:?xt=urn:btih:{info_hash}"));
    release.info_hash = Some(info_hash);
    release
}

async fn start() -> TvWatch {
    let tv = TvWatch::start().await;
    tv.indexer.set_search_results(vec![
        torrent(SCENE_NUMBERED, 's'),
        torrent(ALTERNATE_TITLE, 'a'),
        torrent(NEXT_SCENE_EPISODE, 'n'),
    ]);
    let show_map = json!({"entries": [{
        "scene": {"season": 2, "episode": 1, "absolute": null},
        "tvdb": {"season": 1, "episode": 13, "absolute": null},
    }]});
    xem_cache::put(&tv.server.pool, TVDB_ID, &show_map)
        .await
        .unwrap();
    tv
}

#[derive(Clone, Copy)]
enum Grabber {
    Rss,
    Backlog,
    HoldExpiry,
    WantRoute,
}

/// These paths grab one release per want, so each release gets a want of its
/// own and is the only one the indexer offers while that want searches.
async fn grabbed_titles(grabber: Grabber) -> BTreeSet<String> {
    let tv = start().await;
    let mut grabbed = BTreeSet::new();
    for title in [SCENE_NUMBERED, ALTERNATE_TITLE, NEXT_SCENE_EPISODE] {
        grabbed.extend(grab_alone(&tv, grabber, title).await);
    }
    tv.server.cleanup().await;
    grabbed
}

async fn grab_alone(tv: &TvWatch, grabber: Grabber, title: &str) -> Vec<String> {
    let release = usenet_release(title, title);
    tv.indexer.set_search_results(vec![release.clone()]);
    let state = &tv.server.app_state;
    let pattern_id = match grabber {
        Grabber::Rss => {
            let pattern = tv.pattern(&media_want()).await;
            process_releases(
                state,
                &tv.source,
                &[release],
                std::slice::from_ref(&pattern),
            )
            .await;
            pattern.id
        }
        Grabber::Backlog => {
            let pattern = tv.pattern(&media_want()).await;
            backlog_search_pattern(state, &pattern, SearchGates::AUTOMATIC).await;
            pattern.id
        }
        Grabber::HoldExpiry => {
            let pattern = tv.pattern(&media_want()).await;
            hold_unrelated_release(tv, pattern.id).await;
            process_expired_holds(state).await;
            pattern.id
        }
        Grabber::WantRoute => {
            let response = tv
                .server
                .admin_client()
                .post_json(
                    "/api/v1/want",
                    &json!({
                        "source_system": "kobbler",
                        "source_id": title,
                        "callback_url": "http://localhost:1111/callback",
                        "patterns": ["Kitsune Days S01E13"],
                        "media_want": media_want(),
                        "media_type": "tv",
                        "auto_download": true,
                        "min_score": 0.75,
                    }),
                )
                .await;
            response.assert_ok();
            let body: Value = response.json();
            body["watch_pattern_id"].as_str().unwrap().parse().unwrap()
        }
    };
    let grabbed = tv
        .grabbed(pattern_id)
        .await
        .into_iter()
        .filter(|guid| !guid.starts_with(HELD_GUID))
        .collect();
    // The next want searches the same query, which would otherwise be served
    // this release from the search cache.
    sqlx::query("DELETE FROM search_cache")
        .execute(&tv.server.pool)
        .await
        .unwrap();
    grabbed
}

/// A hold whose final search finds nothing better grabs the held release
/// itself, so that grab is left out of what the search accepted.
async fn hold_unrelated_release(tv: &TvWatch, pattern_id: uuid::Uuid) {
    watch_repo::create_match(
        &tv.server.pool,
        &watch_repo::WatchMatchInput {
            pattern_id,
            indexer_source_id: Some(tv.source.id),
            release_guid: &format!("{HELD_GUID}-{pattern_id}"),
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
}

async fn route_titles(tv: &TvWatch, path: &str, request: Value, key: &str) -> BTreeSet<String> {
    let response = tv.server.admin_client().post_json(path, &request).await;
    response.assert_ok();
    let body: Value = response.json();
    body[key]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            r["title"]
                .as_str()
                .or(r["name"].as_str())
                .unwrap()
                .to_owned()
        })
        .collect()
}

async fn searched_titles() -> Vec<(&'static str, BTreeSet<String>)> {
    let tv = start().await;
    let want = serde_json::to_value(media_want()).unwrap();
    let accepted = vec![
        (
            "search-and-download",
            route_titles(
                &tv,
                "/api/v1/search-and-download",
                json!({"patterns": [], "media_want": want, "media_type": "tv", "min_score": 0.75}),
                "results",
            )
            .await,
        ),
        (
            "providers/releases",
            route_titles(
                &tv,
                "/api/v1/providers/releases/search",
                json!({"media_want": want, "min_score": 0.75}),
                "candidates",
            )
            .await,
        ),
        (
            "stream",
            tv.stream_titles(&media_want()).await.into_iter().collect(),
        ),
    ];
    tv.server.cleanup().await;
    accepted
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_tv_search_entry_point_matches_scene_numbers_and_alternate_titles() {
    let (rss, backlog, hold_expiry, want, searched) = tokio::join!(
        grabbed_titles(Grabber::Rss),
        grabbed_titles(Grabber::Backlog),
        grabbed_titles(Grabber::HoldExpiry),
        grabbed_titles(Grabber::WantRoute),
        searched_titles(),
    );
    let mut accepted = vec![
        ("rss", rss),
        ("backlog", backlog),
        ("hold expiry", hold_expiry),
        ("want", want),
    ];
    accepted.extend(searched);

    let both: BTreeSet<String> = [SCENE_NUMBERED, ALTERNATE_TITLE].map(String::from).into();
    let expected: Vec<_> = accepted
        .iter()
        .map(|(entry, _)| (*entry, both.clone()))
        .collect();
    assert_eq!(accepted, expected);
}

#[tokio::test]
async fn pattern_test_page_previews_the_scene_numbered_target() {
    let tv = start().await;
    let pattern = tv.pattern(&media_want()).await;

    let response = tv
        .server
        .admin_client()
        .post_json(
            &format!("/api/v1/admin/watch/{}/test", pattern.id),
            &json!({"release_string": SCENE_NUMBERED}),
        )
        .await;
    response.assert_ok();
    let session_id = response.json::<Value>()["session_id"]
        .as_str()
        .unwrap()
        .parse()
        .unwrap();

    // The session is flushed on a spawned task.
    let mut detail = None;
    for _ in 0..50 {
        detail = get_session_detail(&tv.server.pool, session_id)
            .await
            .unwrap()
            .filter(|d| !d.releases.is_empty());
        if detail.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    let release = &detail.expect("test session logged").releases[0];
    assert_eq!(
        release.matched_pattern.as_deref(),
        Some("Kitsune Days S02E01")
    );
    tv.server.cleanup().await;
}
