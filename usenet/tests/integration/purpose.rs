use axum::http::StatusCode;
use kob_usenet::vendor::testkit::LoopbackServer;
use usenet_proto::Purpose;

use crate::support::{Fixture, Harness, collect, metric, output, token};

const ATTEMPTS: &str = "kobload_usenet_body_attempts_total";

async fn attempts(harness: &Harness, workload: &str) -> u64 {
    metric(
        &harness.metrics().await,
        ATTEMPTS,
        &[&format!("workload=\"{workload}\"")],
    )
}

#[tokio::test]
async fn play_reads_land_on_the_stream_workload_and_warm_reads_on_warm() {
    let fixture = Fixture::season();
    let nntp = LoopbackServer::start(fixture.articles()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let harness = Harness::new(dir.path(), &nntp).await;
    let release = harness.import(&fixture, "idx:guid").await;

    let read = async |file: &str, purpose| {
        let index = output(&release, file);
        let response = harness
            .stream(
                "GET",
                &token(&release.release_key, index, purpose),
                Some("bytes=40000-"),
            )
            .await;
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(collect(response).await.1, None);
    };

    let (stream, warm) = (
        attempts(&harness, "stream").await,
        attempts(&harness, "warm").await,
    );
    read("Show.S01E01.mkv", Purpose::Play).await;
    let after_play = (
        attempts(&harness, "stream").await,
        attempts(&harness, "warm").await,
    );
    assert!(after_play.0 > stream, "play fetched on the stream workload");
    assert_eq!(after_play.1, warm, "play fetched nothing on warm");

    read("Show.S01E02.mkv", Purpose::Warm).await;
    let after_warm = (
        attempts(&harness, "stream").await,
        attempts(&harness, "warm").await,
    );
    assert!(
        after_warm.1 > after_play.1,
        "warm fetched on the warm workload"
    );
    assert_eq!(after_warm.0, after_play.0, "warm fetched nothing on stream");

    let metrics = harness.metrics().await;
    for name in [
        "kobload_usenet_provider_connections",
        "kobload_usenet_provider_busy",
        "kobload_usenet_body_bytes_total",
        "kobload_usenet_connect_failures_total",
        "kobload_usenet_breaker_trips_total",
    ] {
        assert!(
            metrics
                .lines()
                .any(|l| l.starts_with(&format!("{name}{{provider=\"server a\""))),
            "{name} in {metrics}"
        );
    }
    assert!(
        metric(
            &metrics,
            "kobload_usenet_body_bytes_total",
            &["workload=\"stream\""]
        ) > 0
    );
}
