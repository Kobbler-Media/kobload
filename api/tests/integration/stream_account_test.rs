//! A release Real-Debrid once refused still plays from a copy already on the account,
//! and is only demoted, never hidden: the refusal does not hold for every later add.

use std::time::Duration;

use crate::common::debrid_mock::{MockDebrid, MockRelease};
use crate::common::resolve_fixture::Fixture;
use crate::common::stream_fixtures::hash;
use crate::common::tv_watch::{TvWatch, episode_want, torrent_release};
use kobload_api::debrid::DebridProviderType;
use kobload_api::services::stream::account_index::{Want, serving};
use kobload_api::services::stream::warm_cache::{UnavailableReason, record_availability};
use nats_jobs::types::StreamGroup;

const HELD: &str = "Night.Shift.S01E05.1080p.WEB-DL.x264-AAA";
const FRESH: &str = "Night.Shift.S01E05.480p.HDTV.x264-BBB";
const REFUSED: &str = "Night.Shift.S01E05.720p.WEB-DL.x264-CCC";

#[tokio::test]
async fn test_search_offers_held_copies_first_and_refused_releases_last() {
    let mock = MockDebrid::new(Duration::from_millis(5))
        .with_releases(&[
            (&hash('a'), MockRelease::Refused),
            (&hash('c'), MockRelease::Refused),
        ])
        .with_torrent(&hash('a'), &[1]);
    let tv = TvWatch::start_with_debrid(mock.resolver()).await;
    tv.indexer.set_search_results(vec![
        torrent_release("held", HELD, 'a'),
        torrent_release("fresh", FRESH, 'b'),
        torrent_release("refused", REFUSED, 'c'),
    ]);
    let provider = tv.debrid_provider().await;
    for h in [hash('a'), hash('c')] {
        record_availability(
            &tv.server.pool,
            &h,
            provider,
            Some(UnavailableReason::Blocked),
        )
        .await;
    }

    let options = tv
        .stream_options(&episode_want("Night Shift", "1", "5", None))
        .await;

    assert_eq!(
        options
            .iter()
            .map(|o| (o.release_title.as_str(), o.group))
            .collect::<Vec<_>>(),
        vec![
            (HELD, Some(StreamGroup::Account)),
            (FRESH, Some(StreamGroup::Available)),
            (REFUSED, Some(StreamGroup::Failed)),
        ]
    );
    assert!(options[0].cached);
    tv.server.cleanup().await;
}

const PACK: &[&str] = &[
    "Night.Shift.S01/Night.Shift.S01E05.mkv",
    "Night.Shift.S01/Night.Shift.S01E06.mkv",
];

fn want(hash: &str, provider_file_id: i32) -> Want {
    Want {
        info_hash: hash.to_string(),
        file_name: None,
        provider_file_id: Some(provider_file_id),
    }
}

async fn serving_of(f: &Fixture, wants: &[Want]) -> Vec<usize> {
    let mut served: Vec<usize> = serving(
        f.provider_id,
        f.resolver.get(&DebridProviderType::RealDebrid),
        &f.server.app_state.http_client,
        "key",
        wants,
        Duration::from_secs(2),
    )
    .await
    .into_iter()
    .collect();
    served.sort_unstable();
    served
}

#[tokio::test]
async fn test_a_copy_counts_as_held_only_for_the_files_it_selected() {
    let p = hash('p');
    let f = Fixture::with_mock(
        MockDebrid::new(Duration::from_millis(2))
            .with_pack(&p, PACK)
            .with_torrent(&p, &[2]),
    )
    .await;
    assert_eq!(serving_of(&f, &[want(&p, 1), want(&p, 2)]).await, [1]);
    f.server.cleanup().await;
}

#[tokio::test]
async fn test_our_own_adds_count_as_held_without_listing_again() {
    let f = Fixture::new(2, &[]).await;
    f.resolve(&hash('a')).await.unwrap();
    assert_eq!(serving_of(&f, &[want(&hash('a'), 1)]).await, [0]);
    assert_eq!(f.mock.count("list_torrents", None), 1);
    f.server.cleanup().await;
}
