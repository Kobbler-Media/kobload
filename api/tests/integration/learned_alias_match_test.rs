//! A name remembered from the review queue makes a release every entry point
//! rejected match, for the scope it was remembered for.

use std::collections::BTreeSet;

use crate::common::entry_points::{Grabber, grab_alone, searched, torrent};
use crate::common::sport::motogp_race_want;
use crate::common::tv_watch::{TvWatch, episode_want, usenet_release};
use kobload_api::learned_aliases::{AliasScope, remember};
use kobload_api::watch::Release;

struct Case {
    want: fn() -> nats_jobs::MediaWant,
    title: &'static str,
    scope: AliasScope,
    alias: &'static str,
}

fn tv_want() -> nats_jobs::MediaWant {
    episode_want("Kitsune Days", "1", "13", Some(626_262))
}

const CASES: [Case; 2] = [
    Case {
        want: tv_want,
        title: "Renard.Days.S01E13.1080p.WEB-DL.x264-GRP",
        scope: AliasScope::TvShow,
        alias: "Renard Days",
    },
    Case {
        want: motogp_race_want,
        title: "MotoGP.2026.Nippon.Race.1080p.WEB.h264-VERUM",
        scope: AliasScope::SportLocation,
        alias: "Nippon",
    },
];

/// Grabs go to the usenet client the fixture has; the stream wants torrents.
fn release(case: &Case, hash_char: Option<char>) -> Release {
    let mut release = match hash_char {
        Some(c) => torrent(case.title, c),
        None => usenet_release(case.title, case.title),
    };
    release.publish_date = "2026-10-05".to_string();
    release
}

async fn remember_alias(tv: &TvWatch, case: &Case) {
    remember(&tv.server.pool, &(case.want)(), case.scope, case.alias)
        .await
        .unwrap();
}

type Accepted = Vec<(&'static str, BTreeSet<String>)>;

/// What each entry point accepted for every case, before and after the
/// aliases were remembered.
async fn accepted_by(grabber: Option<Grabber>) -> (Accepted, Accepted) {
    let tv = TvWatch::start().await;
    let run = async |tv: &TvWatch| -> Accepted {
        let mut accepted: Accepted = Vec::new();
        for (case, hash_char) in CASES.iter().zip(['t', 's']) {
            let found = match grabber {
                Some(g) => {
                    let grabbed = grab_alone(tv, g, &(case.want)(), &release(case, None)).await;
                    vec![(g.name(), grabbed.into_iter().collect())]
                }
                None => {
                    tv.indexer
                        .set_search_results(vec![release(case, Some(hash_char))]);
                    searched(tv, &(case.want)()).await
                }
            };
            for (entry, titles) in found {
                match accepted.iter_mut().find(|(e, _)| *e == entry) {
                    Some((_, all)) => all.extend(titles),
                    None => accepted.push((entry, titles)),
                }
            }
        }
        accepted
    };
    let before = run(&tv).await;
    for case in &CASES {
        remember_alias(&tv, case).await;
    }
    let after = run(&tv).await;
    tv.server.cleanup().await;
    (before, after)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_remembered_name_matches_at_every_entry_point() {
    let runs = futures::future::join_all(
        Grabber::ALL
            .map(Some)
            .into_iter()
            .chain([None])
            .map(accepted_by),
    )
    .await;
    let (before, after): (Vec<_>, Vec<_>) = runs.into_iter().unzip();
    let before: Accepted = before.into_iter().flatten().collect();
    let after: Accepted = after.into_iter().flatten().collect();

    let none: Accepted = before.iter().map(|(e, _)| (*e, BTreeSet::new())).collect();
    assert_eq!(before, none, "nothing matches before a name is remembered");
    let every: BTreeSet<String> = CASES.iter().map(|c| c.title.to_string()).collect();
    let all: Accepted = after.iter().map(|(e, _)| (*e, every.clone())).collect();
    assert_eq!(after, all);
}
