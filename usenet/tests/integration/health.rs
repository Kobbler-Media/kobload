use axum::http::StatusCode;
use kob_usenet::vendor::testkit::LoopbackServer;
use usenet_proto::HealthSample;

use crate::support::{Fixture, Harness, json};

#[tokio::test]
async fn a_health_sample_counts_missing_articles_of_an_imported_release() {
    let fixture = Fixture::season();
    let articles: usize = fixture.files.iter().map(|f| f.articles.len()).sum();
    let nntp = LoopbackServer::start(fixture.without(0, 5)).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let harness = Harness::new(dir.path(), &nntp).await;
    let release = harness.import(&fixture, "idx:guid").await;
    let health = |query: &'static str| {
        let uri = format!("/v1/releases/{}/health{query}", release.release_key);
        let harness = &harness;
        async move { harness.control("POST", &uri, None).await }
    };

    let full = health("?percent=100").await;
    assert_eq!(full.status(), StatusCode::OK);
    assert_eq!(
        json::<HealthSample>(full).await,
        HealthSample {
            checked: articles,
            missing: 1
        }
    );

    let sampled = health("?percent=5").await;
    assert_eq!(sampled.status(), StatusCode::OK);
    let sampled: HealthSample = json(sampled).await;
    assert!(
        (1..=articles.div_ceil(20)).contains(&sampled.checked),
        "{sampled:?}"
    );

    let default: HealthSample = json(health("").await).await;
    assert_eq!(
        default.checked, sampled.checked,
        "5% unless asked otherwise"
    );

    let unknown = harness
        .control(
            "POST",
            &format!("/v1/releases/nzb-{}/health", "0".repeat(64)),
            None,
        )
        .await;
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
}
