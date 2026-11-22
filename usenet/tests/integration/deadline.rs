use std::time::{Duration, Instant};

use axum::http::StatusCode;
use kob_usenet::vendor::testkit::LoopbackServer;
use usenet_proto::Purpose;

use crate::support::{ARTICLE, Fixture, Harness, collect, json, output, token};

#[tokio::test]
async fn a_provider_silent_on_the_first_article_answers_a_timeout_code() {
    let fixture = Fixture::season();
    let nntp = LoopbackServer::start(fixture.articles()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let harness = Harness::with_gap(dir.path(), &nntp, Duration::from_millis(300)).await;
    let release = harness.import(&fixture, "idx:guid").await;
    let index = output(&release, "Show.S01E01.mkv");
    nntp.stall_article(
        fixture.message_id(0, 0),
        usize::MAX,
        Duration::from_secs(60),
    );

    let started = Instant::now();
    let response = harness
        .stream(
            "GET",
            &token(&release.release_key, index, Purpose::Play),
            None,
        )
        .await;
    assert_eq!(response.status(), StatusCode::GATEWAY_TIMEOUT);
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
    let body: serde_json::Value = json(response).await;
    assert_eq!(body["error"], "timeout", "{body}");
}

#[tokio::test]
async fn a_silent_provider_ends_the_response_once_the_byte_gap_passes() {
    let fixture = Fixture::feature();
    let nntp = LoopbackServer::start(fixture.articles()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let harness = Harness::with_gap(dir.path(), &nntp, Duration::from_millis(300)).await;
    let release = harness.import(&fixture, "idx:feature").await;
    let index = output(&release, "Feature.2026.mkv");
    let held = 40;
    nntp.stall_article(
        fixture.message_id(0, held),
        usize::MAX,
        Duration::from_secs(60),
    );

    let started = Instant::now();
    let response = harness
        .stream(
            "GET",
            &token(&release.release_key, index, Purpose::Play),
            None,
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let (body, error) = tokio::time::timeout(Duration::from_secs(5), collect(response))
        .await
        .expect("the response ends instead of hanging");

    assert!(error.is_some(), "the body ends with an error");
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
    assert!(body.len() <= held * ARTICLE, "{} bytes", body.len());
    assert!(
        body == fixture.data(0)[..body.len()],
        "sent bytes are the file's prefix"
    );
}
