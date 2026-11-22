use axum::http::StatusCode;
use kob_usenet::vendor::testkit::LoopbackServer;
use usenet_proto::Purpose;

use crate::support::{Fixture, Harness, collect, output, token};

#[tokio::test]
async fn a_restarted_service_streams_an_imported_release_without_importing() {
    let fixture = Fixture::season();
    let nntp = LoopbackServer::start(fixture.articles()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();

    let release = {
        let harness = Harness::new(dir.path(), &nntp).await;
        harness.import(&fixture, "idx:guid").await
    };

    let harness = Harness::new(dir.path(), &nntp).await;
    let index = output(&release, "Show.S01E02.mkv");
    let response = harness
        .stream(
            "GET",
            &token(&release.release_key, index, Purpose::Play),
            None,
        )
        .await;
    assert_eq!(response.status(), StatusCode::OK);
    let (body, error) = collect(response).await;
    assert_eq!(error, None);
    assert!(body == fixture.data(1), "streamed bytes match");
}
