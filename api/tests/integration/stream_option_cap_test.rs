//! A search returns far more usenet than torrent options, every one of them "likely", so
//! the final cut holds usenet to its best few and lets torrents use the remaining slots.

use crate::common::stream_fixtures::{option, usenet_option};
use crate::common::tv_watch::episode_want;
use crate::common::usenet::UsenetRig;
use kobload_api::services::stream::RankProvider;
use kobload_api::services::stream::usenet_pack_slot::{MAX_USENET_OPTIONS, truncate};
use nats_jobs::MediaWant;
use nats_jobs::types::{StreamGroup, StreamOption, StreamSourceType};
use uuid::Uuid;

const LIMIT: usize = 50;

#[derive(Clone, Copy)]
enum Run {
    Usenet(usize),
    Pack,
    Torrents(usize),
    DeadTorrents(usize),
}

fn ranked(runs: &[Run]) -> Vec<StreamOption> {
    let provider = RankProvider { id: Uuid::nil() };
    let mut out = Vec::new();
    let mut push = |mut o: StreamOption, group| {
        o.id = format!("{:04}", out.len());
        o.group = Some(group);
        out.push(o);
    };
    for run in runs {
        match *run {
            Run::Usenet(n) => (0..n).for_each(|i| {
                let title = format!("Cap.Show.S01E05.1080p.WEB-DL.x264-U{i}");
                push(
                    usenet_option(&provider, &format!("u{i}"), &title, 2.0, None),
                    StreamGroup::Available,
                );
            }),
            Run::Pack => push(
                usenet_option(
                    &provider,
                    "pack",
                    "Cap.Show.S01.1080p.WEB-DL.x264-PACK",
                    1.0,
                    None,
                ),
                StreamGroup::Available,
            ),
            Run::Torrents(n) => (0..n).for_each(|i| {
                let title = format!("Cap.Show.S01E05.720p.WEB-DL.x264-T{i}");
                push(
                    option(&provider, &format!("t{i}"), &title, 1.5),
                    StreamGroup::Available,
                );
            }),
            Run::DeadTorrents(n) => (0..n).for_each(|i| {
                let title = format!("Cap.Show.S01E05.720p.WEB-DL.x264-D{i}");
                push(
                    option(&provider, &format!("d{i}"), &title, 1.5),
                    StreamGroup::Uncached,
                );
            }),
        }
    }
    out
}

#[derive(Debug, PartialEq)]
struct Kept {
    usenet: usize,
    pack: bool,
    torrents: usize,
    dead: usize,
}

fn kept(options: &[StreamOption]) -> Kept {
    let is_pack = |o: &StreamOption| o.release_title.contains("-PACK");
    let torrent = |o: &&StreamOption| o.source_type == StreamSourceType::Torrent;
    Kept {
        usenet: options
            .iter()
            .filter(|o| o.source_type == StreamSourceType::Usenet && !is_pack(o))
            .count(),
        pack: options.iter().any(is_pack),
        torrents: options
            .iter()
            .filter(torrent)
            .filter(|o| o.group == Some(StreamGroup::Available))
            .count(),
        dead: options
            .iter()
            .filter(torrent)
            .filter(|o| o.group == Some(StreamGroup::Uncached))
            .count(),
    }
}

type Case<'a> = (&'a str, Vec<Run>, Option<&'a MediaWant>, usize, Kept);

#[test]
fn the_cut_keeps_the_best_usenet_and_fills_with_torrents() {
    use Run::*;
    let episode = episode_want("Cap Show", "1", "5", None);
    let movie: Option<&MediaWant> = None;
    let want = Some(&episode);
    let k = |usenet, pack, torrents, dead| Kept {
        usenet,
        pack,
        torrents,
        dead,
    };
    let cases: Vec<Case> = vec![
        (
            "many usenet, few torrents: usenet backfills",
            vec![Usenet(60), Torrents(6)],
            movie,
            LIMIT,
            k(44, false, 6, 0),
        ),
        (
            "few usenet, many torrents",
            vec![Usenet(5), Torrents(80)],
            movie,
            LIMIT,
            k(5, false, 45, 0),
        ),
        (
            "plenty of both: usenet stops at the cap",
            vec![Usenet(40), Torrents(40)],
            movie,
            LIMIT,
            k(MAX_USENET_OPTIONS, false, 30, 0),
        ),
        (
            "a season pack past the cap keeps its slot on top of it",
            vec![Usenet(30), Pack, Torrents(40)],
            want,
            LIMIT,
            k(MAX_USENET_OPTIONS, true, 29, 0),
        ),
        (
            "a pack ranked below every torrent still gets in",
            vec![Usenet(25), Torrents(60), Pack],
            want,
            LIMIT,
            k(MAX_USENET_OPTIONS, true, 29, 0),
        ),
        (
            "an episode with no torrents keeps the pack among backfilled usenet",
            vec![Usenet(70), Pack],
            want,
            LIMIT,
            k(49, true, 0, 0),
        ),
        (
            "a movie reserves nothing for a pack-shaped title",
            vec![Usenet(30), Pack, Torrents(40)],
            movie,
            LIMIT,
            k(MAX_USENET_OPTIONS, false, 30, 0),
        ),
        (
            "torrents that cannot start do not displace live usenet",
            vec![Usenet(30), DeadTorrents(40)],
            movie,
            LIMIT,
            k(30, false, 0, 20),
        ),
        (
            "a list under the limit is untouched",
            vec![Usenet(44), Torrents(6)],
            movie,
            LIMIT,
            k(44, false, 6, 0),
        ),
        (
            "a limit below the cap",
            vec![Usenet(10), Pack, Torrents(10)],
            want,
            5,
            k(4, true, 0, 0),
        ),
    ];
    for (label, runs, want, limit, expected) in cases {
        let mut options = ranked(&runs);
        let total = options.len();
        let top = options[0].id.clone();
        truncate(&mut options, limit, want);
        assert_eq!(kept(&options), expected, "{label}");
        assert_eq!(options.len(), total.min(limit), "{label}: list length");
        assert_eq!(options[0].id, top, "{label}: the top pick stays first");
        assert!(
            options.windows(2).all(|w| w[0].id < w[1].id),
            "{label}: rank order is preserved"
        );
    }
}

#[tokio::test]
async fn a_real_search_keeps_lower_ranked_torrents_beside_a_flood_of_usenet() {
    let show = "Usenet Flood Show";
    let rig = UsenetRig::start().await;
    let releases = (0..60)
        .map(|i| {
            rig.nzb(
                &format!("g-flood-{i}"),
                &format!("Usenet.Flood.Show.S01E05.1080p.WEB-DL.x264-NZB{i}"),
            )
        })
        .collect();
    rig.tv.indexer.set_search_results(releases);
    crate::common::stream_fixtures::create_provider(&rig.tv.server.pool, "real_debrid").await;
    for (i, c) in ['a', 'b', 'c'].into_iter().enumerate() {
        rig.tv
            .seed_hash_index(&format!("{show} S01E05 720p BluRay x265-TOR{i}"), c)
            .await;
    }

    let options = rig
        .tv
        .stream_options(&episode_want(show, "1", "5", None))
        .await;

    let torrents = options
        .iter()
        .filter(|o| o.source_type == StreamSourceType::Torrent)
        .count();
    assert_eq!((options.len(), torrents), (LIMIT, 3), "{options:#?}");
    rig.tv.server.cleanup().await;
}
