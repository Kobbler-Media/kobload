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

/// kobapp stops walking its options at the first `rate_limited`, so a user resolve waits
/// a throttle out rather than reporting it.
#[tokio::test]
async fn test_user_resolve_outlasts_a_long_rate_limit() {
    let f = Fixture::new(5, &[]).await;
    f.mock.rate_limit_next_adds(5);
    assert!(f.resolve(&hash('a')).await.unwrap().cached);
    assert_eq!(f.mock.count("add_magnet", None), 6);
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
    let mut uncached = f.option(&hash('u'));
    uncached.group = Some(StreamGroup::Uncached);
    f.warm_options(vec![failed, uncached, f.option(&hash('c'))])
        .await
        .unwrap();
    assert_eq!(f.mock.count("add_magnet", Some(&hash('r'))), 0);
    assert_eq!(
        f.mock.count("add_magnet", Some(&hash('u'))),
        0,
        "a probe of a release expected to need downloading only spends an add"
    );
    assert_eq!(f.mock.count("add_magnet", Some(&hash('c'))), 1);
    f.server.cleanup().await;
}

const LOST_PACK: &[&str] = &["Show.S01/Show.S01E01.mkv", "Show.S01/Show.S01E02.mkv"];

async fn lost_file_fixture(release: MockRelease) -> Fixture {
    let f = Fixture::with_mock(
        MockDebrid::new(Duration::from_millis(5))
            .with_releases(&[(&hash('a'), release)])
            .with_pack(&hash('a'), LOST_PACK)
            .with_torrent(&hash('a'), &[1, 2]),
    )
    .await;
    f.mock.lose_held_file(&hash('a'), 1);
    f
}

async fn assert_not_marked_unavailable(f: &Fixture) {
    let known = known_availability(&f.server.pool, f.provider_id, &[hash('a')]).await;
    assert!(
        !matches!(
            known.get(&hash('a')),
            Some(KnownAvailability::Dead | KnownAvailability::Blocked)
        ),
        "a lost file on one copy says nothing about the hash: {known:?}"
    );
}

/// Real-Debrid answers `hoster_unavailable` for a file it lost from an old copy on the
/// account, while a fresh add of the same hash is cached and serves it.
#[tokio::test]
async fn test_lost_file_on_a_held_copy_plays_from_a_fresh_add() {
    let f = lost_file_fixture(MockRelease::Cached).await;

    let played = f.resolve_file(&hash('a'), 0).await.unwrap();
    assert!(played.cached);
    assert!(!played.stream_url.contains("/T1/"), "{}", played.stream_url);
    assert_eq!(f.mock.count("add_magnet", Some(&hash('a'))), 1);
    assert!(
        !f.mock.holds("T1"),
        "the copy that lost the file stays first in line: {:?}",
        f.mock.calls()
    );
    assert_not_marked_unavailable(&f).await;
    f.server.cleanup().await;
}

#[tokio::test]
async fn test_warming_a_lost_file_replaces_the_held_copy() {
    let f = lost_file_fixture(MockRelease::Cached).await;

    f.warm_options(vec![f.file_option(&hash('a'), 0, LOST_PACK[0])])
        .await
        .unwrap();
    assert_eq!(f.mock.count("add_magnet", Some(&hash('a'))), 1);
    assert!(!f.mock.holds("T1"), "{:?}", f.mock.calls());
    assert_not_marked_unavailable(&f).await;

    let before = f.mock.count("unrestrict_link", None);
    assert!(f.resolve_file(&hash('a'), 0).await.unwrap().cached);
    assert_eq!(f.mock.count("unrestrict_link", None), before);
    f.server.cleanup().await;
}

#[tokio::test]
async fn test_lost_file_keeps_the_held_copy_when_the_fresh_add_is_refused() {
    let f = lost_file_fixture(MockRelease::Refused).await;

    assert!(f.resolve_file(&hash('a'), 0).await.is_err());
    assert!(f.mock.holds("T1"), "its other files still play");
    assert!(f.resolve_file(&hash('a'), 1).await.unwrap().cached);
    f.server.cleanup().await;
}

#[tokio::test]
async fn test_a_just_added_torrent_that_404s_is_asked_again() {
    let f = Fixture::new(5, &[]).await;
    f.resolve(&hash('z')).await.unwrap();
    f.mock.not_found_next_gets(2);
    assert!(f.resolve(&hash('a')).await.unwrap().cached);
    assert_eq!(f.mock.count("add_magnet", Some(&hash('a'))), 1);
    f.server.cleanup().await;
}

/// RD sometimes reports an old copy finished with no links; Torrentio adds it again.
#[tokio::test]
async fn test_a_held_copy_without_links_plays_from_a_fresh_add() {
    let f = Fixture::with_mock(
        MockDebrid::new(Duration::from_millis(5)).with_torrent(&hash('a'), &[1]),
    )
    .await;
    f.mock.strip_held_links(&hash('a'));
    let played = f.resolve(&hash('a')).await.unwrap();
    assert!(played.cached, "{played:?}");
    assert_eq!(f.mock.count("add_magnet", Some(&hash('a'))), 1);
    assert!(!f.mock.holds("T1"));
    f.server.cleanup().await;
}

/// Without trackers RD finds peers for an uncached release through DHT alone.
#[tokio::test]
async fn test_magnets_carry_trackers() {
    let f = Fixture::new(5, &[]).await;
    f.resolve(&hash('a')).await.unwrap();
    let mut russian = f.option(&hash('b'));
    russian.release_title = "Щенячий патруль / PAW Patrol / Сезон: 1 [2013 WEB-DL 1080p]".into();
    f.warm_options(vec![russian]).await.unwrap();
    let magnets = f.mock.magnets();
    assert_eq!(magnets.len(), 2, "{magnets:?}");
    for m in &magnets {
        assert!(m.contains("&tr="), "{m}");
    }
    assert!(!magnets[0].contains("t-ru.org"), "{}", magnets[0]);
    assert!(magnets[1].contains("t-ru.org"), "{}", magnets[1]);
    f.server.cleanup().await;
}
