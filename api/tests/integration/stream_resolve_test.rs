use crate::common;

use std::time::{Duration, Instant};

use common::debrid_mock::MockRelease;
use common::resolve_fixture::Fixture;
use common::stream_fixtures::hash;
use kobload_api::services::stream::warm_cache::{UnavailableReason, record_availability};
use kobload_api::services::stream::{KnownAvailability, known_availability};

#[tokio::test]
async fn test_known_blocked_refuses_in_ms_without_provider_calls() {
    let f = Fixture::new(20, &[]).await;
    record_availability(
        &f.server.pool,
        &hash('a'),
        f.provider_id,
        Some(UnavailableReason::Blocked),
    )
    .await;

    let started = Instant::now();
    let err = f.resolve(&hash('a')).await.unwrap_err();
    assert!(
        started.elapsed() < Duration::from_millis(250),
        "{:?}",
        started.elapsed()
    );
    assert!(
        err.contains("refused this"),
        "kobbler classifies on this text: {err}"
    );
    assert!(f.mock.calls().is_empty(), "{:?}", f.mock.calls());
    f.server.cleanup().await;
}

#[tokio::test]
async fn test_resolves_share_one_account_listing() {
    let f = Fixture::new(5, &[]).await;
    f.resolve(&hash('a')).await.unwrap();
    f.resolve(&hash('b')).await.unwrap();
    assert_eq!(f.mock.count("list_torrents", None), 1);
    f.server.cleanup().await;
}

#[tokio::test]
async fn test_repeat_resolve_of_a_ready_hash_is_free() {
    let f = Fixture::new(5, &[]).await;
    let first = f.resolve(&hash('a')).await.unwrap();
    assert!(first.cached);
    let before = f.mock.calls().len();
    let second = f.resolve(&hash('a')).await.unwrap();
    assert_eq!(second.stream_url, first.stream_url);
    assert_eq!(f.mock.calls().len(), before, "{:?}", f.mock.calls());
    f.server.cleanup().await;
}

#[tokio::test]
async fn test_cached_resolve_does_not_wait_out_a_fixed_sleep() {
    let f = Fixture::new(0, &[]).await;
    let started = Instant::now();
    assert!(f.resolve(&hash('a')).await.unwrap().cached);
    assert!(
        started.elapsed() < Duration::from_millis(450),
        "{:?}",
        started.elapsed()
    );
    f.server.cleanup().await;
}

#[tokio::test]
async fn test_warm_and_resolve_of_one_hash_share_one_add() {
    let f = Fixture::new(40, &[]).await;
    let warm = f.warm(&[&hash('a')]);
    tokio::time::sleep(Duration::from_millis(60)).await;
    let resolved = f.resolve(&hash('a')).await.unwrap();
    warm.await.unwrap();
    assert!(resolved.cached);
    assert_eq!(f.mock.count("add_magnet", Some(&hash('a'))), 1);
    assert_eq!(f.mock.count("unrestrict_link", Some(&hash('a'))), 1);
    assert_eq!(f.mock.torrents_for(&hash('a')), 1);
    f.server.cleanup().await;
}

#[tokio::test]
async fn test_warm_keeps_an_uncached_torrent_the_user_is_waiting_on() {
    let f = Fixture::new(40, &[(&hash('b'), MockRelease::Uncached)]).await;
    let warm = f.warm(&[&hash('a'), &hash('b')]);
    tokio::time::sleep(Duration::from_millis(60)).await;
    let resolved = f.resolve(&hash('b')).await.unwrap();
    warm.await.unwrap();
    assert!(!resolved.cached);
    assert!(resolved.torrent_id.is_some());
    assert_eq!(f.mock.count("add_magnet", Some(&hash('b'))), 1);
    assert_eq!(
        f.mock.torrents_for(&hash('b')),
        1,
        "warming must not delete what the user is polling"
    );
    f.server.cleanup().await;
}

#[tokio::test]
async fn test_rate_limited_resolve_honours_retry_after() {
    let f = Fixture::new(5, &[]).await;
    f.mock.rate_limit_next_adds(1);
    let resolved = f.resolve(&hash('a')).await.unwrap();
    assert!(resolved.cached);
    let adds: Vec<Instant> = f
        .mock
        .calls()
        .into_iter()
        .filter(|c| c.op == "add_magnet")
        .map(|c| c.started)
        .collect();
    assert_eq!(adds.len(), 2);
    assert!(adds[1] - adds[0] >= f.mock.retry_after);
    f.server.cleanup().await;
}

#[tokio::test]
async fn test_warm_yields_to_a_user_resolve() {
    let warm_hashes: Vec<String> = ['1', '2', '3', '4'].map(hash).to_vec();
    let f = Fixture::new(40, &[]).await;
    let refs: Vec<&str> = warm_hashes.iter().map(String::as_str).collect();
    let warm = f.warm(&refs);
    tokio::time::sleep(Duration::from_millis(120)).await;

    f.resolve(&hash('u')).await.unwrap();
    let user_finished = Instant::now();
    warm.await.unwrap();

    let calls = f.mock.calls();
    let user_started = calls
        .iter()
        .find(|c| c.hash == hash('u'))
        .map(|c| c.started)
        .unwrap();
    let grace = Duration::from_millis(15);
    let interleaved: Vec<_> = calls
        .into_iter()
        .filter(|c| c.hash != hash('u') && !c.hash.is_empty())
        .filter(|c| c.started > user_started + grace && c.started < user_finished)
        .collect();
    assert!(
        interleaved.is_empty(),
        "warm calls ran during a user resolve: {interleaved:?}"
    );
    f.server.cleanup().await;
}

#[tokio::test]
async fn test_warm_verdicts_reach_the_next_resolve_and_search() {
    let f = Fixture::new(
        5,
        &[
            (&hash('r'), MockRelease::Refused),
            (&hash('u'), MockRelease::Uncached),
        ],
    )
    .await;
    f.warm(&[&hash('c'), &hash('r'), &hash('u')]).await.unwrap();

    let known = known_availability(
        &f.server.pool,
        f.provider_id,
        &[hash('c'), hash('r'), hash('u')],
    )
    .await;
    assert_eq!(known.get(&hash('c')), Some(&KnownAvailability::Available));
    assert_eq!(known.get(&hash('r')), Some(&KnownAvailability::Blocked));
    assert_eq!(known.get(&hash('u')), Some(&KnownAvailability::Dead));
    assert_eq!(
        f.mock.torrents_for(&hash('u')),
        0,
        "probe slots are handed back"
    );
    assert_eq!(
        f.mock.torrents_for(&hash('c')),
        1,
        "the ready probe stays for playback"
    );

    let adds = f.mock.count("add_magnet", None);
    let started = Instant::now();
    assert!(
        f.resolve(&hash('r'))
            .await
            .unwrap_err()
            .contains("refused this")
    );
    assert!(started.elapsed() < Duration::from_millis(250));
    let ready = f.resolve(&hash('c')).await.unwrap();
    assert!(ready.cached);
    assert_eq!(f.mock.count("add_magnet", None), adds);
    f.server.cleanup().await;
}
