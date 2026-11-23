//! A usenet pack's files are known only once the engine imports it, so an episode stream
//! search offers the season pack itself and resolving it with the episode streams just
//! that file.

use std::collections::BTreeSet;

use reqwest::StatusCode;
use serde_json::json;

use crate::common::entry_points::{Grabber, grab_alone, torrent};
use crate::common::stream_fixtures::create_provider;
use crate::common::tv_watch::{TvWatch, episode_want, usenet_release};
use crate::common::usenet::UsenetRig;
use nats_jobs::types::{StreamOption, StreamSourceType};

const GB: u64 = 1_000_000_000;

fn pack_title(show: &str) -> String {
    format!("{}.S02.1080p.WEB-DL.x264-NZB", show.replace(' ', "."))
}

fn of_type(options: &[StreamOption], source: StreamSourceType) -> Vec<&StreamOption> {
    options.iter().filter(|o| o.source_type == source).collect()
}

async fn episode_options(rig: &UsenetRig, show: &str) -> Vec<StreamOption> {
    rig.tv
        .stream_options(&episode_want(show, "2", "3", None))
        .await
}

#[tokio::test]
async fn test_a_usenet_season_pack_streams_the_wanted_episode() {
    let show = "Usenet Pack Show";
    let rig = UsenetRig::start().await;
    let dotted = show.replace(' ', ".");
    let layout: Vec<(String, u64)> = (1..=5)
        .map(|e| (format!("{dotted}.S02E{e:02}.1080p.mkv"), GB))
        .collect();
    let layout: Vec<(&str, u64)> = layout.iter().map(|(p, s)| (p.as_str(), *s)).collect();
    rig.engine.layout("g-season-pack", &layout);
    rig.tv
        .indexer
        .set_search_results(vec![rig.nzb("g-season-pack", &pack_title(show))]);

    let options = episode_options(&rig, show).await;
    let usenet = of_type(&options, StreamSourceType::Usenet);
    assert_eq!(usenet.len(), 1, "{options:?}");
    assert_eq!(usenet[0].nzb_ref, Some(rig.nzb_ref("g-season-pack")));

    let (status, resolved) = rig
        .resolve(json!({
            "nzb_ref": usenet[0].nzb_ref,
            "release_title": usenet[0].release_title,
            "episode": { "season": 2, "episode": 3 },
        }))
        .await;
    assert_eq!(status, StatusCode::OK, "{resolved}");
    assert_eq!(resolved["file_id"], 2);
    assert_eq!(resolved["filename"], format!("{dotted}.S02E03.1080p.mkv"));

    let (status, missing) = rig
        .resolve(json!({
            "nzb_ref": usenet[0].nzb_ref,
            "episode": { "season": 2, "episode": 9 },
        }))
        .await;
    assert!(!status.is_success(), "{missing}");
    assert!(
        missing.as_str().unwrap().contains("S02E09 was not found"),
        "{missing}"
    );

    let options = episode_options(&rig, show).await;
    let imported = of_type(&options, StreamSourceType::Usenet);
    assert_eq!(imported.len(), 1, "{options:?}");
    assert_eq!(
        imported[0].release_key.as_deref(),
        resolved["release_key"].as_str()
    );
    assert_eq!(imported[0].files.len(), 5, "{:?}", imported[0].files);
    assert_eq!(imported[0].files[0].id, 2, "the wanted episode leads");
    rig.tv.server.cleanup().await;
}

#[tokio::test]
async fn test_a_usenet_pack_of_another_season_is_not_offered() {
    let show = "Usenet Other Season Show";
    let rig = UsenetRig::start().await;
    rig.tv.indexer.set_search_results(vec![rig.nzb(
        "g-first-season-pack",
        &format!("{}.S01.1080p.WEB-DL.x264-NZB", show.replace(' ', ".")),
    )]);

    let options = episode_options(&rig, show).await;
    assert!(options.is_empty(), "{options:?}");
    rig.tv.server.cleanup().await;
}

#[tokio::test]
async fn test_a_single_episode_release_ranks_above_the_pack() {
    let show = "Usenet Rank Show";
    let rig = UsenetRig::start().await;
    rig.tv.indexer.set_search_results(vec![
        rig.nzb("g-season-pack", &pack_title(show)),
        rig.nzb(
            "g-single-episode",
            &format!("{}.S02E03.1080p.WEB-DL.x264-NZB", show.replace(' ', ".")),
        ),
    ]);

    let options = episode_options(&rig, show).await;
    let refs: Vec<_> = of_type(&options, StreamSourceType::Usenet)
        .iter()
        .map(|o| o.nzb_ref.clone())
        .collect();
    assert_eq!(
        refs,
        vec![
            Some(rig.nzb_ref("g-single-episode")),
            Some(rig.nzb_ref("g-season-pack"))
        ],
        "{options:?}"
    );
    rig.tv.server.cleanup().await;
}

#[tokio::test]
async fn test_torrent_season_packs_are_still_refused_for_an_episode() {
    let show = "Torrent Pack Show";
    let rig = UsenetRig::start().await;
    create_provider(&rig.tv.server.pool, "real_debrid").await;
    rig.tv
        .seed_hash_index(&format!("{show} S02 1080p WEB-DL x264-TOR"), 'h')
        .await;
    rig.tv.indexer.set_search_results(vec![
        rig.nzb("g-season-pack", &pack_title(show)),
        torrent(
            &format!("{}.S02.720p.WEB-DL.x264-TOR", show.replace(' ', ".")),
            'i',
        ),
    ]);

    let options = episode_options(&rig, show).await;
    assert!(
        of_type(&options, StreamSourceType::Torrent).is_empty(),
        "{options:?}"
    );
    assert_eq!(
        of_type(&options, StreamSourceType::Usenet).len(),
        1,
        "{options:?}"
    );
    rig.tv.server.cleanup().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn test_library_wants_still_refuse_a_usenet_pack_for_an_episode() {
    let show = "Library Pack Show";
    let single = format!("{}.S02E03.1080p.WEB-DL.x264-NZB", show.replace(' ', "."));
    let tv = TvWatch::start().await;
    let want = episode_want(show, "2", "3", None);
    let mut grabbed = Vec::new();
    for grabber in Grabber::ALL {
        let mut titles = BTreeSet::new();
        for title in [pack_title(show), single.clone()] {
            titles.extend(grab_alone(&tv, grabber, &want, &usenet_release(&title, &title)).await);
        }
        grabbed.push((grabber.name(), titles));
    }
    tv.server.cleanup().await;

    for (grabber, titles) in grabbed {
        assert_eq!(
            titles,
            BTreeSet::from([single.clone()]),
            "{grabber} grabbed the wrong releases"
        );
    }
}

#[tokio::test]
async fn test_the_pack_survives_a_full_page_of_single_episodes() {
    let show = "Usenet Crowded Show";
    let rig = UsenetRig::start().await;
    let dotted = show.replace(' ', ".");
    let mut releases: Vec<_> = (0..60)
        .map(|i| {
            rig.nzb(
                &format!("g-single-{i}"),
                &format!("{dotted}.S02E03.1080p.WEB-DL.x264-NZB{i}"),
            )
        })
        .collect();
    releases.push(rig.nzb("g-season-pack", &pack_title(show)));
    rig.tv.indexer.set_search_results(releases);

    let options = episode_options(&rig, show).await;
    assert_eq!(options.len(), 50, "{options:?}");
    let usenet = of_type(&options, StreamSourceType::Usenet);
    assert_eq!(
        usenet.last().map(|o| o.nzb_ref.clone()),
        Some(Some(rig.nzb_ref("g-season-pack"))),
        "{options:?}"
    );
    rig.tv.server.cleanup().await;
}

#[tokio::test]
async fn test_an_episode_search_also_asks_the_indexer_for_the_season() {
    let show = "Usenet Episode Scoped Show";
    let rig = UsenetRig::start().await;
    rig.tv.indexer.set_search_results(vec![rig.nzb(
        "g-single-episode",
        &format!("{}.S02E03.1080p.WEB-DL.x264-NZB", show.replace(' ', ".")),
    )]);
    rig.tv.indexer.set_season_only_results(vec![
        rig.nzb("g-season-pack", &pack_title(show)),
        rig.nzb(
            "g-single-episode",
            &format!("{}.S02E03.1080p.WEB-DL.x264-NZB", show.replace(' ', ".")),
        ),
    ]);

    let options = episode_options(&rig, show).await;
    let refs: Vec<_> = of_type(&options, StreamSourceType::Usenet)
        .iter()
        .map(|o| o.nzb_ref.clone())
        .collect();
    assert_eq!(
        refs,
        vec![
            Some(rig.nzb_ref("g-single-episode")),
            Some(rig.nzb_ref("g-season-pack"))
        ],
        "{options:?}"
    );
    assert_eq!(rig.tv.indexer.searches(), 2);

    let cached = episode_options(&rig, show).await;
    assert_eq!(
        of_type(&cached, StreamSourceType::Usenet).len(),
        2,
        "{cached:?}"
    );
    assert_eq!(rig.tv.indexer.searches(), 2);
    rig.tv.server.cleanup().await;
}
