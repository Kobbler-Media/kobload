use std::time::Duration;

use axum::http::StatusCode;
use kob_usenet::vendor::testkit::LoopbackServer;
use usenet_proto::Purpose;

use crate::support::{ARTICLE, Fixture, Harness, collect, json, metric, output, token};

const FAILURES: &str = "kobload_usenet_stream_failures_total";

#[tokio::test]
async fn a_missing_article_in_the_first_chunk_is_refused_with_a_code() {
    let fixture = Fixture::season();
    let nntp = LoopbackServer::start(fixture.without(0, 5)).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let harness = Harness::new(dir.path(), &nntp).await;
    let release = harness.import(&fixture, "idx:guid").await;
    let index = output(&release, "Show.S01E01.mkv");

    let response = harness
        .stream(
            "GET",
            &token(&release.release_key, index, Purpose::Play),
            None,
        )
        .await;
    assert_eq!(response.status(), StatusCode::BAD_GATEWAY);
    let body: serde_json::Value = json(response).await;
    assert_eq!(body["error"], "missing_articles", "{body}");
    let metrics = harness.metrics().await;
    assert_eq!(metric(&metrics, FAILURES, &["missing_articles"]), 1);
}

#[tokio::test]
async fn an_article_missing_mid_body_aborts_without_filler_and_is_counted() {
    let fixture = Fixture::feature();
    let gone = 40;
    let nntp = LoopbackServer::start(fixture.without(0, gone))
        .await
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let harness = Harness::new(dir.path(), &nntp).await;
    let release = harness.import(&fixture, "idx:feature").await;
    let index = output(&release, "Feature.2026.mkv");

    let response = harness
        .stream(
            "GET",
            &token(&release.release_key, index, Purpose::Play),
            None,
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let (body, error) = tokio::time::timeout(Duration::from_secs(20), collect(response))
        .await
        .expect("the response ends");

    assert!(
        error.is_some(),
        "the body ends with an error, {} bytes sent",
        body.len()
    );
    assert!(!body.is_empty(), "the first chunk was sent");
    assert!(body.len() <= gone * ARTICLE, "{} bytes", body.len());
    assert!(!body.contains(&0), "no filler bytes");
    assert!(
        body == fixture.data(0)[..body.len()],
        "sent bytes are the file's prefix"
    );
    let metrics = harness.metrics().await;
    assert_eq!(metric(&metrics, FAILURES, &["missing_articles"]), 1);

    let resumed = harness
        .stream(
            "GET",
            &token(&release.release_key, index, Purpose::Play),
            Some(&format!("bytes={}-", body.len())),
        )
        .await;
    assert_eq!(
        resumed.status(),
        StatusCode::BAD_GATEWAY,
        "a resume at the damage gets the code before any byte"
    );
}
