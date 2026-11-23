use axum::http::StatusCode;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use kob_usenet::vendor::testkit::LoopbackServer;
use usenet_proto::ImportResponse;

use std::sync::Arc;
use std::time::Duration;

use crate::support::{Fixture, Harness, collect, import_body, json};

#[tokio::test]
async fn an_import_by_cache_key_reuses_the_cached_nzb() {
    let fixture = Fixture::season();
    let nntp = LoopbackServer::start(fixture.articles()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let harness = Harness::new(dir.path(), &nntp).await;

    let first = harness.import(&fixture, "idx:guid").await;
    let paths: Vec<_> = first
        .outputs
        .iter()
        .map(|o| (o.index, o.path.as_str(), o.size))
        .collect();
    assert_eq!(
        paths,
        [
            (0, "Show.S01E01.mkv", fixture.data(0).len() as u64),
            (1, "Show.S01E02.mkv", fixture.data(1).len() as u64),
        ]
    );

    let again = harness
        .control("POST", "/v1/import", Some(import_body(None, "idx:guid")))
        .await;
    assert_eq!(again.status(), StatusCode::OK);
    let again: ImportResponse = json(again).await;
    assert_eq!(again.release_key, first.release_key);
    assert_eq!(again.outputs, first.outputs);

    let known = harness
        .control("GET", &format!("/v1/releases/{}", first.release_key), None)
        .await;
    assert_eq!(known.status(), StatusCode::OK);
    assert_eq!(json::<ImportResponse>(known).await.outputs, first.outputs);

    let unknown = harness
        .control("GET", &format!("/v1/releases/nzb-{}", "0".repeat(64)), None)
        .await;
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
    let malformed = harness.control("GET", "/v1/releases/abc", None).await;
    assert_eq!(malformed.status(), StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn import_failures_map_to_statuses() {
    let fixture = Fixture::season();
    let nntp = LoopbackServer::start(fixture.without(0, 0)).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let harness = Harness::new(dir.path(), &nntp).await;

    let uncached = harness
        .control(
            "POST",
            "/v1/import",
            Some(import_body(None, "idx:never-seen")),
        )
        .await;
    assert_eq!(uncached.status(), StatusCode::NOT_FOUND);
    assert_eq!(
        json::<serde_json::Value>(uncached).await,
        serde_json::json!({"error": "not_cached"})
    );

    let mut bad = import_body(None, "idx:bad");
    bad["nzb_base64"] = "!!!".into();
    let bad = harness.control("POST", "/v1/import", Some(bad)).await;
    assert_eq!(bad.status(), StatusCode::BAD_REQUEST);

    let missing = harness
        .control(
            "POST",
            "/v1/import",
            Some(import_body(Some(&fixture), "idx:guid")),
        )
        .await;
    assert_eq!(missing.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let body: serde_json::Value = serde_json::from_slice(&collect(missing).await.0).unwrap();
    assert_eq!(body["error"], "missing_articles");
    assert!(
        body["message"].as_str().is_some_and(|m| !m.is_empty()),
        "{body}"
    );
}

#[tokio::test]
async fn a_release_with_nothing_importable_is_refused_not_an_engine_fault() {
    let fixture = Fixture::sidecars_only();
    let nntp = LoopbackServer::start(fixture.articles()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let harness = Harness::new(dir.path(), &nntp).await;

    let response = harness
        .control(
            "POST",
            "/v1/import",
            Some(import_body(Some(&fixture), "idx:nfo")),
        )
        .await;
    let status = response.status();
    let body: serde_json::Value = json(response).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY, "{body}");
    assert_eq!(body["error"], "unimportable", "{body}");
    assert!(
        body["message"].as_str().is_some_and(|m| !m.is_empty()),
        "{body}"
    );
}

#[tokio::test]
async fn a_large_nzb_is_accepted() {
    let fixture = Fixture::season();
    let nntp = LoopbackServer::start(fixture.articles()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let harness = Harness::new(dir.path(), &nntp).await;
    let mut body = import_body(None, "idx:large");
    body["nzb_base64"] = STANDARD.encode(fixture.padded_nzb(24 << 20)).into();

    let response = harness.control("POST", "/v1/import", Some(body)).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(json::<ImportResponse>(response).await.outputs.len(), 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn imports_past_the_admission_cap_wait_for_a_slot() {
    let fixture = Arc::new(Fixture::season());
    let nntp = LoopbackServer::start(fixture.articles()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let harness = Arc::new(Harness::new(dir.path(), &nntp).await);
    let (arrived, release) = nntp.hold_article(fixture.message_id(0, 0));

    let held: Vec<_> = (0..4)
        .map(|n| {
            let harness = Arc::clone(&harness);
            let fixture = Arc::clone(&fixture);
            tokio::spawn(async move {
                harness
                    .control(
                        "POST",
                        "/v1/import",
                        Some(import_body(Some(&fixture), &format!("idx:held-{n}"))),
                    )
                    .await
                    .status()
            })
        })
        .collect();
    arrived.await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;

    let mut bad = import_body(None, "idx:bad");
    bad["nzb_base64"] = "!!!".into();
    let queued = {
        let harness = Arc::clone(&harness);
        tokio::spawn(async move {
            harness
                .control("POST", "/v1/import", Some(bad))
                .await
                .status()
        })
    };
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(!queued.is_finished(), "a fifth import waits while four run");

    drop(release);
    for import in held {
        assert_eq!(import.await.unwrap(), StatusCode::OK);
    }
    assert_eq!(queued.await.unwrap(), StatusCode::BAD_REQUEST);
}
