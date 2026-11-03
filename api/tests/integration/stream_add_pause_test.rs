use crate::common;

use std::time::Duration;

use common::debrid_mock::{MockDebrid, MockRelease};
use common::resolve_fixture::Fixture;
use common::stream_fixtures::hash;
use common::tv_watch::{TvWatch, episode_want, torrent_release};
use kobload_api::services::stream::{KnownAvailability, known_availability, vouched};
use nats_jobs::types::StreamGroup;

const HOLD: Duration = Duration::from_millis(60);

async fn pausing(releases: &[(&str, MockRelease)]) -> Fixture {
    Fixture::with_mock(
        MockDebrid::new(Duration::from_millis(5))
            .with_releases(releases)
            .with_add_pauses(HOLD),
    )
    .await
}

async fn known(f: &Fixture, h: &str) -> Option<KnownAvailability> {
    known_availability(&f.server.pool, f.provider_id, &[h.to_string()])
        .await
        .get(h)
        .copied()
}

#[tokio::test]
async fn test_a_refusal_while_rd_pauses_adds_is_retried_after_the_hold() {
    let f = pausing(&[]).await;
    f.mock.refuse_next_adds(1);
    assert!(f.resolve(&hash('a')).await.unwrap().cached);
    assert_eq!(f.mock.count("add_magnet", Some(&hash('a'))), 2);
    assert_eq!(
        known(&f, &hash('a')).await,
        Some(KnownAvailability::Available)
    );
    f.server.cleanup().await;
}

#[tokio::test]
async fn test_a_release_still_refused_after_the_hold_is_recorded_refused() {
    let f = pausing(&[(&hash('a'), MockRelease::Refused)]).await;
    let err = f.resolve(&hash('a')).await.unwrap_err();
    assert!(err.contains("refused"), "{err}");
    assert_eq!(f.mock.count("add_magnet", Some(&hash('a'))), 2);
    assert_eq!(
        known(&f, &hash('a')).await,
        Some(KnownAvailability::Blocked)
    );
    f.server.cleanup().await;
}

#[tokio::test]
async fn test_a_name_rd_always_refuses_is_not_held_for() {
    let f = pausing(&[(&hash('a'), MockRelease::Refused)]).await;
    f.resolve_titled(&hash('a'), TAGGED_NAME).await.unwrap_err();
    assert_eq!(f.mock.count("add_magnet", Some(&hash('a'))), 1);
    f.server.cleanup().await;
}

const TAGGED_NAME: &str = "Show.S01E01.1080p.WEB-DL.DDP5.1-GRP";

#[tokio::test]
async fn test_an_rd_plus_release_gets_the_pause_retry_despite_its_name() {
    let f = pausing(&[]).await;
    vouched::remember(f.provider_id, &hash('a'));
    f.mock.refuse_next_adds(1);
    assert!(
        f.resolve_titled(&hash('a'), TAGGED_NAME)
            .await
            .unwrap()
            .cached
    );
    assert_eq!(f.mock.count("add_magnet", Some(&hash('a'))), 2);
    f.server.cleanup().await;
}

#[tokio::test]
async fn test_an_rd_plus_release_refused_again_after_the_pause_counts_as_refused() {
    let f = pausing(&[(&hash('a'), MockRelease::Refused)]).await;
    vouched::remember(f.provider_id, &hash('a'));
    f.resolve_titled(&hash('a'), TAGGED_NAME).await.unwrap_err();
    assert_eq!(f.mock.count("add_magnet", Some(&hash('a'))), 2);
    assert_eq!(
        known(&f, &hash('a')).await,
        Some(KnownAvailability::Blocked)
    );
    f.server.cleanup().await;
}

#[tokio::test]
async fn test_warming_leaves_the_account_quiet_for_the_user() {
    let f = pausing(&[]).await;
    let warmed: Vec<String> = ['1', '2', '3', '4'].map(hash).to_vec();
    let refs: Vec<&str> = warmed.iter().map(String::as_str).collect();
    f.warm(&refs).await.unwrap();
    assert_eq!(f.mock.count("add_magnet", None), 1, "{:?}", f.mock.calls());
    let probed = known_availability(&f.server.pool, f.provider_id, &warmed).await;
    assert_eq!(probed.len(), 1, "{probed:?}");
    assert!(f.resolve(&hash('u')).await.unwrap().cached);
    f.server.cleanup().await;
}

#[tokio::test]
async fn test_a_warming_refusal_during_a_pause_records_nothing() {
    let f = Fixture::with_mock(
        MockDebrid::new(Duration::from_millis(5)).with_add_pauses(Duration::from_secs(30)),
    )
    .await;
    f.mock.refuse_next_adds(1);
    let mut option = f.option(&hash('a'));
    option.release_title = "Show.S01E01.1080p.BluRay.x265-GRP".to_string();
    f.warm_options(vec![option]).await.unwrap();
    assert_eq!(f.mock.count("add_magnet", None), 1);
    assert_eq!(known(&f, &hash('a')).await, None);
    f.server.cleanup().await;
}

async fn full_account(stale: bool) -> Fixture {
    let f = Fixture::with_mock(
        MockDebrid::new(Duration::from_millis(5))
            .with_releases(&[
                (&hash('u'), MockRelease::Uncached),
                (&hash('v'), MockRelease::Uncached),
            ])
            .with_active_limit(1),
    )
    .await;
    assert!(!f.resolve(&hash('u')).await.unwrap().cached);
    if stale {
        sqlx::query(
            "UPDATE debrid_stream_torrents SET touched_at = NOW() - INTERVAL '1 hour' WHERE info_hash = $1",
        )
        .bind(hash('u'))
        .execute(&f.server.pool)
        .await
        .unwrap();
    }
    f
}

#[tokio::test]
async fn test_a_full_account_frees_the_slot_of_a_stream_nobody_came_back_for() {
    let f = full_account(true).await;
    assert!(!f.resolve(&hash('v')).await.unwrap().cached);
    assert_eq!(f.mock.torrents_for(&hash('u')), 0);
    assert_eq!(f.mock.torrents_for(&hash('v')), 1);
    f.server.cleanup().await;
}

#[tokio::test]
async fn test_a_recent_stream_keeps_its_slot() {
    let f = full_account(false).await;
    let err = f.resolve(&hash('v')).await.unwrap_err();
    assert!(err.contains("Too many active"), "{err}");
    assert_eq!(f.mock.torrents_for(&hash('u')), 1);
    f.server.cleanup().await;
}

#[tokio::test]
async fn test_coming_back_for_a_stream_keeps_its_slot() {
    for poll in [false, true] {
        let f = full_account(true).await;
        if poll {
            let torrent_id: String = sqlx::query_scalar(
                "SELECT torrent_id FROM debrid_stream_torrents WHERE info_hash = $1",
            )
            .bind(hash('u'))
            .fetch_one(&f.server.pool)
            .await
            .unwrap();
            f.poll_status(&format!("rd_{torrent_id}")).await;
        } else {
            assert!(!f.resolve(&hash('u')).await.unwrap().cached);
        }
        let err = f.resolve(&hash('v')).await.unwrap_err();
        assert!(err.contains("Too many active"), "poll {poll}: {err}");
        assert_eq!(f.mock.torrents_for(&hash('u')), 1, "poll {poll}");
        f.server.cleanup().await;
    }
}

#[tokio::test]
async fn test_only_a_file_that_is_not_a_video_by_name_and_type_fails() {
    let f = Fixture::new(5, &[]).await;
    f.mock
        .serve_as(&hash('a'), "setup.exe", "application/x-msdownload");
    f.mock
        .serve_as(&hash('b'), "Show.S01E01.mkv", "application/octet-stream");
    let err = f.resolve(&hash('a')).await.unwrap_err();
    assert!(err.contains("not a video"), "{err}");
    assert!(f.resolve(&hash('b')).await.unwrap().cached);
    f.server.cleanup().await;
}

#[tokio::test]
async fn test_a_name_rd_always_refuses_ranks_below_one_it_takes() {
    const TAGGED: &str = "Day.Watch.S02E03.1080p.WEB-DL.x264-AAA";
    const CLEAN: &str = "Day.Watch.S02E03.720p.WEB.h264-BBB";
    let mock = MockDebrid::new(Duration::from_millis(5)).with_add_pauses(HOLD);
    let tv = TvWatch::start_with_debrid(mock.resolver()).await;
    tv.indexer.set_search_results(vec![
        torrent_release("tagged", TAGGED, 'a'),
        torrent_release("clean", CLEAN, 'b'),
    ]);

    let options = tv
        .stream_options(&episode_want("Day Watch", "2", "3", None))
        .await;

    assert_eq!(
        options
            .iter()
            .map(|o| (o.release_title.as_str(), o.group))
            .collect::<Vec<_>>(),
        vec![
            (CLEAN, Some(StreamGroup::Available)),
            (TAGGED, Some(StreamGroup::Uncached)),
        ]
    );
    tv.server.cleanup().await;
}
