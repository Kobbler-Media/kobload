use crate::common;

use std::time::{Duration, Instant};

use common::debrid_mock::{MockDebrid, MockRelease};
use common::resolve_fixture::Fixture;
use common::stream_fixtures::hash;
use kobload_api::services::stream::warm_cache::{UnavailableReason, record_availability};
use kobload_api::services::stream::{KnownAvailability, known_availability};
use nats_jobs::types::StreamGroup;

#[tokio::test]
async fn test_known_blocked_hash_held_on_the_account_plays_from_that_copy() {
    let f = Fixture::with_mock(
        MockDebrid::new(Duration::from_millis(5))
            .with_releases(&[(&hash('a'), MockRelease::Refused)])
            .with_torrent(&hash('a'), &[1]),
    )
    .await;
    record_availability(
        &f.server.pool,
        &hash('a'),
        f.provider_id,
        Some(UnavailableReason::Blocked),
    )
    .await;

    let played = f.resolve(&hash('a')).await.unwrap();
    assert!(played.cached);
    assert_eq!(f.mock.count("add_magnet", None), 0, "{:?}", f.mock.calls());
    assert_eq!(
        known_availability(&f.server.pool, f.provider_id, &[hash('a')])
            .await
            .get(&hash('a')),
        Some(&KnownAvailability::Available)
    );
    f.server.cleanup().await;
}

#[tokio::test]
async fn test_known_blocked_hash_is_asked_again_and_cleared_once_accepted() {
    let f = Fixture::new(5, &[]).await;
    record_availability(
        &f.server.pool,
        &hash('a'),
        f.provider_id,
        Some(UnavailableReason::Blocked),
    )
    .await;

    assert!(f.resolve(&hash('a')).await.unwrap().cached);
    assert_eq!(f.mock.count("add_magnet", Some(&hash('a'))), 1);
    assert_eq!(
        known_availability(&f.server.pool, f.provider_id, &[hash('a')])
            .await
            .get(&hash('a')),
        Some(&KnownAvailability::Available)
    );
    f.server.cleanup().await;
}

#[tokio::test]
async fn test_refusal_is_reported_in_the_text_kobbler_classifies() {
    let f = Fixture::new(5, &[(&hash('a'), MockRelease::Refused)]).await;
    let err = f.resolve(&hash('a')).await.unwrap_err();
    assert!(
        err.contains("refused this"),
        "kobbler classifies on this text: {err}"
    );
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
    assert!(
        f.resolve(&hash('r'))
            .await
            .unwrap_err()
            .contains("refused this")
    );
    assert_eq!(
        f.mock.count("add_magnet", None),
        adds + 1,
        "a refusal demotes the release; playing it asks the provider again"
    );
    let ready = f.resolve(&hash('c')).await.unwrap();
    assert!(ready.cached);
    assert_eq!(f.mock.count("add_magnet", None), adds + 1);
    f.server.cleanup().await;
}

/// The incident: a warmed link the debrid host stopped serving was handed out for every
/// resolve of its release for hours, twenty failed plays in a row, because the warm entry
/// outlived every report that it did not work.
#[tokio::test]
async fn test_a_reported_failure_drops_the_warmed_link() {
    let f = Fixture::new(5, &[]).await;
    f.resolve_file(&hash('a'), 16).await.unwrap();
    f.resolve_file(&hash('a'), 16).await.unwrap();
    let unrestricts = || f.mock.count("unrestrict_link", Some(&hash('a')));
    assert_eq!(
        unrestricts(),
        1,
        "a second resolve did not reuse the warmed link"
    );

    f.server
        .admin_client()
        .post_json(
            "/api/v1/stream/outcome",
            &serde_json::json!({
                "debrid_provider_id": f.provider_id,
                "info_hash": hash('a').to_uppercase(),
                "file_id": 16,
                "ok": false,
                "reason": "probe_failed",
            }),
        )
        .await
        .assert_status(reqwest::StatusCode::NO_CONTENT);

    f.resolve_file(&hash('a'), 16).await.unwrap();
    assert_eq!(
        unrestricts(),
        2,
        "the link reported dead was handed out again"
    );
    f.server.cleanup().await;
}

#[tokio::test]
async fn test_warming_skips_releases_that_already_failed() {
    let f = Fixture::new(5, &[(&hash('r'), MockRelease::Refused)]).await;
    let mut failed = f.option(&hash('r'));
    failed.group = Some(StreamGroup::Failed);
    f.warm_options(vec![failed, f.option(&hash('c'))])
        .await
        .unwrap();
    assert_eq!(f.mock.count("add_magnet", Some(&hash('r'))), 0);
    assert_eq!(f.mock.count("add_magnet", Some(&hash('c'))), 1);
    f.server.cleanup().await;
}
