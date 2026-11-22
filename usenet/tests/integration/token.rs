use axum::http::StatusCode;
use kob_usenet::vendor::testkit::LoopbackServer;
use usenet_proto::Purpose;

use crate::support::{Fixture, Harness, SECRET, now, output, signed};

#[tokio::test]
async fn stream_tokens_are_checked_before_any_read() {
    let fixture = Fixture::season();
    let nntp = LoopbackServer::start(fixture.articles()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let harness = Harness::new(dir.path(), &nntp).await;
    let release = harness.import(&fixture, "idx:guid").await;
    let key = release.release_key.as_str();
    let index = output(&release, "Show.S01E01.mkv");
    let unknown = format!("nzb-{}", "0".repeat(64));
    let later = now() + 3600;

    let cases = [
        (
            "forged",
            signed(b"not-the-secret", key, index, Purpose::Play, later),
            StatusCode::FORBIDDEN,
        ),
        (
            "expired",
            signed(SECRET, key, index, Purpose::Play, now() - 1),
            StatusCode::FORBIDDEN,
        ),
        ("garbage", "not-a-token".to_owned(), StatusCode::FORBIDDEN),
        (
            "wrong output",
            signed(SECRET, key, 9, Purpose::Play, later),
            StatusCode::NOT_FOUND,
        ),
        (
            "unknown release",
            signed(SECRET, &unknown, 0, Purpose::Play, later),
            StatusCode::NOT_FOUND,
        ),
        (
            "not a release key",
            signed(SECRET, "abc", 0, Purpose::Play, later),
            StatusCode::NOT_FOUND,
        ),
        (
            "valid",
            signed(SECRET, key, index, Purpose::Play, later),
            StatusCode::OK,
        ),
    ];
    for (label, token, status) in cases {
        for method in ["GET", "HEAD"] {
            let response = harness.stream(method, &token, None).await;
            assert_eq!(response.status(), status, "{method} {label}");
        }
    }
}
