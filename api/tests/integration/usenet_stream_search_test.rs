//! Hash-index and Torrentio torrents nearly always exist, so a usenet search that only ran
//! when they found nothing never listed anything for our engine to stream.

use std::time::{Duration, Instant};

use crate::common::stream_fixtures::{create_provider, hash};
use crate::common::tv_watch::{TvWatch, episode_want};
use crate::common::usenet::UsenetRig;
use kobload_api::repositories::hash_index::{self, HashIndexEntry};
use nats_jobs::types::{StreamOption, StreamSourceType};

async fn seed_torrent(tv: &TvWatch, title: &str, hash_char: char) {
    hash_index::upsert_batch(
        &tv.server.pool,
        &[HashIndexEntry {
            info_hash: hash(hash_char),
            imdb_id: None,
            tmdb_id: None,
            original_title: title.to_string(),
            parsed_title: None,
            year: None,
            quality: None,
            size: Some(2_000_000_000),
            magnet_url: None,
            source: "test".to_string(),
            seeders: Some(50),
            season: None,
            episodes: None,
        }],
    )
    .await
    .unwrap();
}

async fn torrent_and_usenet_rig(show: &str) -> UsenetRig {
    let rig = UsenetRig::start().await;
    create_provider(&rig.tv.server.pool, "real_debrid").await;
    let dotted = show.replace(' ', ".");
    seed_torrent(
        &rig.tv,
        &format!("{show} S01E05 1080p WEB-DL x264-TOR"),
        'c',
    )
    .await;
    rig.tv.indexer.set_search_results(vec![rig.nzb(
        "g-usenet",
        &format!("{dotted}.S01E05.1080p.WEB-DL.x264-NZB"),
    )]);
    rig
}

fn of_type(options: &[StreamOption], source: StreamSourceType) -> Vec<&StreamOption> {
    options.iter().filter(|o| o.source_type == source).collect()
}

#[tokio::test]
async fn test_usenet_options_are_listed_beside_fast_torrent_candidates() {
    let rig = torrent_and_usenet_rig("Beside Torrents Show").await;

    let options = rig
        .tv
        .stream_options(&episode_want("Beside Torrents Show", "1", "5", None))
        .await;

    assert_eq!(
        of_type(&options, StreamSourceType::Torrent).len(),
        1,
        "{options:?}"
    );
    let usenet = of_type(&options, StreamSourceType::Usenet);
    assert_eq!(usenet.len(), 1, "{options:?}");
    assert_eq!(usenet[0].debrid_provider_id, rig.provider);
    assert_eq!(usenet[0].nzb_ref, Some(rig.nzb_ref("g-usenet")));
    assert_eq!(rig.tv.indexer.searches(), 1);
    rig.tv.server.cleanup().await;
}

#[tokio::test]
async fn test_no_usenet_provider_spends_no_indexer_search() {
    let tv = TvWatch::start().await;
    create_provider(&tv.server.pool, "real_debrid").await;
    seed_torrent(
        &tv,
        "No Usenet Provider Show S01E05 1080p WEB-DL x264-TOR",
        'd',
    )
    .await;
    tv.indexer
        .set_search_results(vec![crate::common::tv_watch::usenet_release(
            "g-unused",
            "No.Usenet.Provider.Show.S01E05.1080p.WEB-DL.x264-NZB",
        )]);

    let options = tv
        .stream_options(&episode_want("No Usenet Provider Show", "1", "5", None))
        .await;

    assert_eq!(
        of_type(&options, StreamSourceType::Torrent).len(),
        1,
        "{options:?}"
    );
    assert_eq!(tv.indexer.searches(), 0);
    tv.server.cleanup().await;
}

#[tokio::test]
async fn test_slow_usenet_indexer_is_cut_at_the_deadline_and_cached_for_the_next_search() {
    let rig = torrent_and_usenet_rig("Slow Usenet Show").await;
    let indexer_delay = Duration::from_secs(5);
    rig.tv.indexer.set_search_delay(indexer_delay);
    let want = episode_want("Slow Usenet Show", "1", "5", None);

    let started = Instant::now();
    let first = rig.tv.stream_options(&want).await;
    let took = started.elapsed();

    assert!(
        took < indexer_delay,
        "search waited {took:?} for the indexer"
    );
    assert_eq!(
        of_type(&first, StreamSourceType::Torrent).len(),
        1,
        "{first:?}"
    );
    assert!(
        of_type(&first, StreamSourceType::Usenet).is_empty(),
        "{first:?}"
    );

    tokio::time::sleep(indexer_delay.saturating_sub(took) + Duration::from_millis(500)).await;
    let second = rig.tv.stream_options(&want).await;

    assert_eq!(
        of_type(&second, StreamSourceType::Usenet).len(),
        1,
        "the late results serve the next search: {second:?}"
    );
    assert_eq!(rig.tv.indexer.searches(), 1);
    rig.tv.server.cleanup().await;
}
