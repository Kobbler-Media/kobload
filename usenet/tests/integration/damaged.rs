use std::time::Duration;

use axum::http::StatusCode;
use kob_usenet::vendor::testkit::LoopbackServer;
use usenet_proto::Purpose;

use crate::support::{ARTICLE, Fixture, Harness, collect, output, token};

#[tokio::test]
async fn an_article_missing_everywhere_aborts_the_body_without_filler() {
    let fixture = Fixture::season();
    let gone = 5;
    let nntp = LoopbackServer::start(fixture.without(0, gone))
        .await
        .unwrap();
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
    assert_eq!(response.status(), StatusCode::OK);
    let (body, error) = tokio::time::timeout(Duration::from_secs(20), collect(response))
        .await
        .expect("the response ends");

    assert!(
        error.is_some(),
        "the body ends with an error, {} bytes sent",
        body.len()
    );
    assert!(body.len() <= gone * ARTICLE, "{} bytes", body.len());
    assert!(!body.contains(&0), "no filler bytes");
    assert!(
        body == fixture.data(0)[..body.len()],
        "sent bytes are the file's prefix"
    );
}
