use crate::common;

use std::time::Duration;

use common::debrid_mock::MockRelease;
use common::resolve_fixture::Fixture;
use common::stream_fixtures::hash;
use futures::StreamExt;
use kobload_api::jobs::handlers::CacheCleanupHandler;
use kobload_api::jobs::worker::JobHandler;
use kobload_api::services::stream::warm_cache::{UnavailableReason, record_availability};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

async fn logged(latency_ms: u64, releases: &[(&str, MockRelease)]) -> Fixture {
    let f = Fixture::new(latency_ms, releases).await;
    f.enable_debrid_log().await;
    f
}

#[tokio::test]
async fn test_resolve_records_each_outcome() {
    let f = logged(
        0,
        &[
            (&hash('b'), MockRelease::Uncached),
            (&hash('c'), MockRelease::Refused),
        ],
    )
    .await;
    let cases: &[(char, &str, Value, &[&str])] = &[
        (
            'a',
            "ready",
            Value::Bool(true),
            &["requested", "added", "selected", "cache_check", "ready"],
        ),
        (
            'b',
            "caching",
            Value::Bool(false),
            &["requested", "added", "selected", "cache_check", "caching"],
        ),
        ('c', "refused", Value::Null, &["requested", "refused"]),
    ];
    for (c, status, cached, events) in cases {
        f.http_resolve(&hash(*c)).await;
        let attempt = f.attempt(&hash(*c), "play").await;
        assert_eq!(attempt["status"], *status, "{c}: {attempt}");
        assert_eq!(attempt["cached"], *cached, "{c}: {attempt}");
        assert_eq!(f.event_types(&attempt).await, *events, "{c}");
    }
    let refused = f.attempt(&hash('c'), "play").await;
    assert!(refused["error"].as_str().unwrap().contains("refused this"));
    f.server.cleanup().await;
}

#[tokio::test]
async fn test_client_polling_tracks_caching_to_ready() {
    let f = logged(0, &[(&hash('b'), MockRelease::Uncached)]).await;
    let resolved = f.http_resolve(&hash('b')).await;
    let torrent_id = resolved["torrent_id"].as_str().expect("pending torrent id");

    f.mock.set_progress(&hash('b'), 0.4);
    f.poll_status(torrent_id).await;
    let caching = f.attempt(&hash('b'), "play").await;
    assert_eq!(caching["status"], "caching");
    assert!((caching["progress"].as_f64().unwrap() - 0.4).abs() < 1e-6);
    assert!(caching["ready_at"].is_null());

    f.mock.set_release(&hash('b'), MockRelease::Cached);
    assert_eq!(f.poll_status(torrent_id).await["status"], "ready");
    let ready = f.attempt(&hash('b'), "play").await;
    assert_eq!(ready["status"], "ready");
    assert_eq!(ready["progress"], 1.0);
    assert!(!ready["ready_at"].is_null());
    assert_eq!(f.event_types(&ready).await.last().unwrap(), "ready");

    f.poll_status(torrent_id).await;
    let events = f.event_types(&ready).await;
    assert_eq!(
        events.iter().filter(|e| *e == "ready").count(),
        1,
        "{events:?}"
    );
    f.server.cleanup().await;
}

#[tokio::test]
async fn test_known_blocked_is_logged_without_provider_calls() {
    let f = logged(0, &[]).await;
    record_availability(
        &f.server.pool,
        &hash('a'),
        f.provider_id,
        Some(UnavailableReason::Blocked),
    )
    .await;
    f.http_resolve(&hash('a')).await;
    let attempt = f.attempt(&hash('a'), "play").await;
    assert_eq!(attempt["status"], "refused");
    assert_eq!(
        f.event_types(&attempt).await,
        ["requested", "known_blocked"]
    );
    f.server.cleanup().await;
}

#[tokio::test]
async fn test_nothing_is_recorded_while_the_debrid_log_is_off() {
    let f = Fixture::new(0, &[]).await;
    f.http_resolve(&hash('a')).await;
    f.warm(&[&hash('b')]).await.unwrap();
    assert!(f.attempts().await.is_empty());
    f.server.cleanup().await;
}

#[tokio::test]
async fn test_warm_probes_record_what_was_cached() {
    let f = logged(
        0,
        &[
            (&hash('r'), MockRelease::Refused),
            (&hash('u'), MockRelease::Uncached),
        ],
    )
    .await;
    f.warm(&[&hash('a'), &hash('r'), &hash('u')]).await.unwrap();
    let cases = [
        ('a', "ready", Value::Bool(true), "ready"),
        ('r', "refused", Value::Null, "refused"),
        ('u', "uncached", Value::Bool(false), "released"),
    ];
    for (c, status, cached, last_event) in cases {
        let attempt = f.attempt(&hash(c), "warm").await;
        assert_eq!(attempt["status"], status, "{c}: {attempt}");
        assert_eq!(attempt["cached"], cached, "{c}: {attempt}");
        assert!(attempt["release_title"].as_str().is_some(), "{attempt}");
        assert_eq!(f.event_types(&attempt).await.last().unwrap(), last_event);
    }
    f.server.cleanup().await;
}

#[tokio::test]
async fn test_resolve_joining_a_warm_probe_gets_its_own_outcome() {
    let f = logged(40, &[]).await;
    let warm = f.warm(&[&hash('a')]);
    tokio::time::sleep(Duration::from_millis(60)).await;
    f.http_resolve(&hash('a')).await;
    warm.await.unwrap();
    let play = f.attempt(&hash('a'), "play").await;
    assert_eq!(play["status"], "ready");
    assert_eq!(f.event_types(&play).await, ["requested", "joined", "ready"]);
    assert_eq!(f.attempt(&hash('a'), "warm").await["status"], "ready");
    f.server.cleanup().await;
}

#[tokio::test]
async fn test_playback_reports_close_the_attempt() {
    let f = logged(0, &[]).await;
    let cases = [
        ('a', true, None, "played", "played"),
        (
            'b',
            false,
            Some("decoder error"),
            "failed",
            "playback_failed",
        ),
    ];
    for (c, ok, reason, status, event) in cases {
        f.http_resolve(&hash(c)).await;
        f.report(&hash(c), ok, reason).await;
        let attempt = f.attempt(&hash(c), "play").await;
        assert_eq!(attempt["status"], status, "{c}: {attempt}");
        assert_eq!(f.event_types(&attempt).await.last().unwrap(), event);
        if let Some(reason) = reason {
            assert!(attempt["error"].as_str().unwrap().contains(reason));
        }
    }
    f.server.cleanup().await;
}

#[tokio::test]
async fn test_cleanup_purges_after_a_day_and_abandons_stalled_attempts() {
    let f = logged(0, &[(&hash('s'), MockRelease::Uncached)]).await;
    f.http_resolve(&hash('o')).await;
    f.http_resolve(&hash('s')).await;
    f.http_resolve(&hash('n')).await;
    let pool = &f.server.pool;
    sqlx::query(
        "UPDATE stream_attempts SET created_at = NOW() - INTERVAL '25 hours' WHERE info_hash = $1",
    )
    .bind(hash('o'))
    .execute(pool)
    .await
    .unwrap();
    sqlx::query("UPDATE stream_attempts SET updated_at = NOW() - INTERVAL '11 minutes' WHERE info_hash = $1")
        .bind(hash('s'))
        .execute(pool)
        .await
        .unwrap();

    CacheCleanupHandler::new(pool.clone())
        .execute(
            uuid::Uuid::new_v4(),
            serde_json::json!({}),
            CancellationToken::new(),
        )
        .await
        .unwrap();

    let attempts = f.attempts().await;
    assert!(attempts.iter().all(|a| a["info_hash"] != hash('o')));
    let stalled = f.attempt(&hash('s'), "play").await;
    assert_eq!(stalled["status"], "abandoned");
    assert_eq!(f.event_types(&stalled).await.last().unwrap(), "abandoned");
    assert_eq!(f.attempt(&hash('n'), "play").await["status"], "ready");
    f.server.cleanup().await;
}

#[tokio::test]
async fn test_live_stream_carries_new_attempts_and_their_progress() {
    let f = logged(0, &[]).await;
    let resp = reqwest::Client::new()
        .get(format!(
            "{}/api/v1/stream-attempts/stream",
            f.server.api_url
        ))
        .bearer_auth(&f.server.admin_token)
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success());
    let mut body = resp.bytes_stream();
    let mut seen = String::new();
    let first = tokio::time::timeout(Duration::from_secs(2), body.next()).await;
    seen.push_str(&String::from_utf8_lossy(&first.unwrap().unwrap().unwrap()));
    assert!(seen.contains(r#""type":"snapshot""#), "{seen}");

    f.http_resolve(&hash('z')).await;
    let wanted = [r#""type":"created""#, r#""status":"ready""#, &hash('z')];
    let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
    while !wanted.iter().all(|w| seen.contains(w)) {
        let chunk = tokio::time::timeout_at(deadline, body.next())
            .await
            .unwrap_or_else(|_| panic!("stream never carried the attempt: {seen}"));
        seen.push_str(&String::from_utf8_lossy(&chunk.unwrap().unwrap()));
    }
    f.server.cleanup().await;
}

#[tokio::test]
async fn test_clear_removes_every_attempt() {
    let f = logged(0, &[]).await;
    f.http_resolve(&hash('a')).await;
    assert_eq!(f.attempts().await.len(), 1);
    f.server
        .admin_client()
        .delete("/api/v1/stream-attempts")
        .await
        .assert_ok();
    assert!(f.attempts().await.is_empty());
    f.server.cleanup().await;
}
