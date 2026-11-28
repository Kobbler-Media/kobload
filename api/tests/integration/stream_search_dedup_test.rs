//! A details page searches and the server prewarms the same title a second later, and kobbler
//! web and kobapp may each ask too: overlapping searches for one title must not each query
//! the indexers.

use std::time::Duration;

use crate::common::stream_fixtures::create_provider;
use crate::common::tv_watch::{TvWatch, episode_want, torrent_release};
use crate::common::usenet::UsenetRig;
use nats_jobs::types::StreamSourceType;

#[tokio::test]
async fn test_overlapping_searches_query_the_usenet_indexers_once() {
    let rig = UsenetRig::start().await;
    create_provider(&rig.tv.server.pool, "real_debrid").await;
    rig.tv
        .seed_hash_index("Overlap Usenet Show S01E05 1080p WEB-DL x264-TOR", 'c')
        .await;
    rig.tv.indexer.set_search_results(vec![rig.nzb(
        "g-usenet",
        "Overlap.Usenet.Show.S01E05.1080p.WEB-DL.x264-NZB",
    )]);
    rig.tv.indexer.set_search_delay(Duration::from_millis(500));
    let want = episode_want("Overlap Usenet Show", "1", "5", None);

    let (first, second) = tokio::join!(rig.tv.stream_options(&want), async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        rig.tv.stream_options(&want).await
    });

    for options in [&first, &second] {
        assert_eq!(
            options
                .iter()
                .filter(|o| o.source_type == StreamSourceType::Usenet)
                .count(),
            1,
            "{options:?}"
        );
    }
    assert_eq!(
        rig.tv.indexer.searches(),
        2,
        "one episode query and one season query between both searches"
    );
    rig.tv.server.cleanup().await;
}

#[tokio::test]
async fn test_overlapping_searches_query_the_torrent_indexers_once() {
    let tv = TvWatch::start().await;
    create_provider(&tv.server.pool, "real_debrid").await;
    tv.indexer.set_search_results(vec![torrent_release(
        "t-1",
        "Overlap.Torrent.Show.S01E05.1080p.WEB-DL.x264-AAA",
        'a',
    )]);
    tv.indexer.set_search_delay(Duration::from_millis(500));
    let want = episode_want("Overlap Torrent Show", "1", "5", None);

    let (first, second) = tokio::join!(tv.stream_options(&want), async {
        tokio::time::sleep(Duration::from_millis(100)).await;
        tv.stream_options(&want).await
    });

    assert_eq!(first.len(), 1, "{first:?}");
    assert_eq!(second.len(), 1, "{second:?}");
    assert_eq!(tv.indexer.searches(), 1);
    tv.server.cleanup().await;
}
