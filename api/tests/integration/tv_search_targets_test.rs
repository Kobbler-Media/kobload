//! Every TV search entry point must match a release by its XEM scene numbering
//! and by the want's alternate titles.
//!
//! The want is TVDB S01E13, which the scene numbers S02E01. Scene S02E02 is
//! the next episode and must still be rejected.

use std::collections::BTreeSet;

use serde_json::{Value, json};

use crate::common::entry_points::{Grabber, grab_alone, searched, torrent};
use crate::common::tv_watch::{TvWatch, episode_want, usenet_release};
use kobload_api::decision_log::repository::get_session_detail;
use kobload_api::repositories::xem_cache;

const TVDB_ID: i64 = 515_151;
const SCENE_NUMBERED: &str = "Kitsune.Days.S02E01.1080p.WEB-DL.x264-GRP";
const ALTERNATE_TITLE: &str = "Renard.Days.S01E13.1080p.WEB-DL.x264-GRP";
const NEXT_SCENE_EPISODE: &str = "Kitsune.Days.S02E02.1080p.WEB-DL.x264-GRP";

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

async fn grabbed_titles(grabber: Grabber) -> BTreeSet<String> {
    let tv = start().await;
    let mut grabbed = BTreeSet::new();
    for title in [SCENE_NUMBERED, ALTERNATE_TITLE, NEXT_SCENE_EPISODE] {
        grabbed
            .extend(grab_alone(&tv, grabber, &media_want(), &usenet_release(title, title)).await);
    }
    tv.server.cleanup().await;
    grabbed
}

async fn searched_titles() -> Vec<(&'static str, BTreeSet<String>)> {
    let tv = start().await;
    let accepted = searched(&tv, &media_want()).await;
    tv.server.cleanup().await;
    accepted
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_tv_search_entry_point_matches_scene_numbers_and_alternate_titles() {
    let (grabbed, searched) = tokio::join!(
        futures::future::join_all(
            Grabber::ALL.map(|g| async move { (g.name(), grabbed_titles(g).await) })
        ),
        searched_titles(),
    );
    let mut accepted = grabbed;
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
