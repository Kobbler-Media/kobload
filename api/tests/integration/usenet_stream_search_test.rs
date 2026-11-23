//! Hash-index and Torrentio torrents nearly always exist, so a usenet search that only ran
//! when they found nothing never listed anything for our engine to stream.

use std::time::{Duration, Instant};

use crate::common::stream_fixtures::create_provider;
use crate::common::tv_watch::{TvWatch, episode_want};
use crate::common::usenet::UsenetRig;
use nats_jobs::types::{StreamOption, StreamSourceType};

async fn torrent_and_usenet_rig(show: &str) -> UsenetRig {
    let rig = UsenetRig::start().await;
    create_provider(&rig.tv.server.pool, "real_debrid").await;
    let dotted = show.replace(' ', ".");
    rig.tv
        .seed_hash_index(&format!("{show} S01E05 1080p WEB-DL x264-TOR"), 'c')
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
    assert_eq!(
        rig.tv.indexer.searches(),
        2,
        "the episode query and one season query"
    );
    rig.tv.server.cleanup().await;
}

#[tokio::test]
async fn test_no_usenet_provider_spends_no_indexer_search() {
    let tv = TvWatch::start().await;
    create_provider(&tv.server.pool, "real_debrid").await;
    tv.seed_hash_index("No Usenet Provider Show S01E05 1080p WEB-DL x264-TOR", 'd')
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
    assert_eq!(
        rig.tv.indexer.searches(),
        2,
        "the episode query and one season query"
    );
    rig.tv.server.cleanup().await;
}

const SLOW_INDEXER: Duration = Duration::from_secs(6);

fn usenet_titles(options: &[StreamOption]) -> Vec<String> {
    let mut titles: Vec<_> = of_type(options, StreamSourceType::Usenet)
        .iter()
        .map(|o| o.release_title.clone())
        .collect();
    titles.sort();
    titles
}

struct SlowAndFast {
    rig: UsenetRig,
    want: nats_jobs::MediaWant,
    fast: String,
    both: Vec<String>,
}

// The slow indexer outranks the fast one, so it is the one a source-by-source search
// would be stuck on when the deadline hits.
async fn slow_and_fast_indexers(show: &str) -> SlowAndFast {
    let rig = torrent_and_usenet_rig(show).await;
    let dotted = show.replace(' ', ".");
    let fast_source = rig.add_indexer("Fast Newznab", -1).await;
    let slow = format!("{dotted}.S01E05.1080p.WEB-DL.x264-SLOW");
    let fast = format!("{dotted}.S01E05.1080p.WEB-DL.x264-FAST");
    let indexer = &rig.tv.indexer;
    indexer.set_source_results(rig.tv.source.id, vec![rig.nzb("g-slow", &slow)]);
    indexer.set_source_results(
        fast_source,
        vec![rig.nzb_from(fast_source, "g-fast", &fast)],
    );
    indexer.set_source_delay(rig.tv.source.id, SLOW_INDEXER);
    indexer.set_source_delay(fast_source, Duration::from_millis(50));
    let mut both = vec![fast.clone(), slow];
    both.sort();
    SlowAndFast {
        want: episode_want(show, "1", "5", None),
        rig,
        fast,
        both,
    }
}

#[tokio::test]
async fn test_fast_indexer_is_listed_at_the_deadline_and_the_slow_one_on_the_next_search() {
    let SlowAndFast {
        rig,
        want,
        fast,
        both,
    } = slow_and_fast_indexers("Partial Usenet Show").await;

    let started = Instant::now();
    let first = rig.tv.stream_options(&want).await;
    let took = started.elapsed();

    assert!(
        took < SLOW_INDEXER,
        "search waited {took:?} for the indexer"
    );
    assert_eq!(usenet_titles(&first), vec![fast], "{first:?}");
    assert_eq!(
        of_type(&first, StreamSourceType::Torrent).len(),
        1,
        "{first:?}"
    );

    tokio::time::sleep(SLOW_INDEXER.saturating_sub(took) + Duration::from_millis(500)).await;
    let second = rig.tv.stream_options(&want).await;

    assert_eq!(usenet_titles(&second), both, "{second:?}");
    assert_eq!(
        rig.tv.indexer.searches(),
        4,
        "two indexers, each with an episode and a season query"
    );
    rig.tv.server.cleanup().await;
}

#[tokio::test]
async fn test_partial_result_is_not_cached_as_complete() {
    let SlowAndFast {
        rig,
        want,
        fast,
        both,
    } = slow_and_fast_indexers("Unpoisoned Usenet Show").await;

    let first = rig.tv.stream_options(&want).await;
    assert_eq!(usenet_titles(&first), vec![fast.clone()], "{first:?}");
    assert_eq!(rig.tv.indexer.searches(), 4);

    let during = rig.tv.stream_options(&want).await;
    assert_eq!(usenet_titles(&during), vec![fast], "{during:?}");
    assert_eq!(
        rig.tv.indexer.searches(),
        8,
        "a cache miss searches again, as it did before partial results"
    );

    let after = rig.tv.stream_options(&want).await;
    assert_eq!(usenet_titles(&after), both, "{after:?}");
    assert_eq!(
        rig.tv.indexer.searches(),
        8,
        "the complete result is served from the cache"
    );
    rig.tv.server.cleanup().await;
}

async fn assert_fast_query_serves_the_deadline(
    show: &str,
    episode_delay: Duration,
    season_delay: Duration,
    listed_first: &str,
) {
    let rig = UsenetRig::start().await;
    create_provider(&rig.tv.server.pool, "real_debrid").await;
    rig.tv
        .seed_hash_index(&format!("{show} S01E05 1080p WEB-DL x264-TOR"), 'e')
        .await;
    let dotted = show.replace(' ', ".");
    let episode_title = format!("{dotted}.S01E05.1080p.WEB-DL.x264-NZB");
    let pack_title = format!("{dotted}.S01.1080p.WEB-DL.x264-NZB");
    let episode = rig.nzb("g-episode", &episode_title);
    let pack = rig.nzb("g-pack", &pack_title);
    rig.tv.indexer.set_search_results(vec![episode.clone()]);
    rig.tv.indexer.set_season_only_results(vec![pack, episode]);
    rig.tv.indexer.set_query_delays(episode_delay, season_delay);
    let want = episode_want(show, "1", "5", None);

    let started = Instant::now();
    let first = rig.tv.stream_options(&want).await;
    let took = started.elapsed();

    assert!(
        took < SLOW_INDEXER,
        "search waited {took:?} for the indexer"
    );
    let expected_first = if listed_first == "episode" {
        vec![episode_title.clone()]
    } else {
        let mut titles = vec![pack_title.clone(), episode_title.clone()];
        titles.sort();
        titles
    };
    assert_eq!(usenet_titles(&first), expected_first, "{first:?}");

    tokio::time::sleep(SLOW_INDEXER.saturating_sub(took) + Duration::from_millis(500)).await;
    let second = rig.tv.stream_options(&want).await;

    let mut complete = vec![pack_title, episode_title];
    complete.sort();
    assert_eq!(
        usenet_titles(&second),
        complete,
        "de-duplicated by listing: {second:?}"
    );
    assert_eq!(rig.tv.indexer.searches(), 2);
    rig.tv.server.cleanup().await;
}

#[tokio::test]
async fn test_episode_query_answers_at_the_deadline_with_the_season_query_slow() {
    assert_fast_query_serves_the_deadline(
        "Slow Season Query Show",
        Duration::from_millis(50),
        SLOW_INDEXER,
        "episode",
    )
    .await;
}

#[tokio::test]
async fn test_season_query_answers_at_the_deadline_with_the_episode_query_slow() {
    assert_fast_query_serves_the_deadline(
        "Slow Episode Query Show",
        SLOW_INDEXER,
        Duration::from_millis(50),
        "season",
    )
    .await;
}
