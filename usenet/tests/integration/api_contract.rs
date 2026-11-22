use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use kob_usenet::vendor::testkit::LoopbackServer;
use reqwest::StatusCode;
use usenet_proto::{HealthSample, JobRequest, JobState, JobStatus};

use crate::support::{CONTROL, Fixture, Harness};

// Each call is built the way kobload-api's `UsenetEngine` builds it
// (api/src/usenet/engine.rs), over a real socket, so a drift in paths, query encoding or
// status codes shows here rather than in production.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_engine_answers_every_call_kobload_api_makes_the_way_it_reads_them() {
    let fixture = Fixture::feature();
    let season = Fixture::season();
    let nntp = LoopbackServer::start([fixture.articles(), season.articles()].concat())
        .await
        .unwrap();
    let dir = tempfile::tempdir().unwrap();
    let harness = Harness::new(dir.path(), &nntp).await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = harness.app.clone();
    tokio::spawn(async move { axum::serve(listener, app).await.ok() });
    let http = reqwest::Client::new();
    let jobs = format!("{base}/v1/jobs");

    let job = JobRequest {
        id: "0196f0a2-7c3e-7b51-9d0e-3f1b2a4c5d6e".to_owned(),
        name: "Feature.2026.1080p.WEB-DL-GRP".to_owned(),
        category: "kobload".to_owned(),
        nzb_base64: STANDARD.encode(fixture.nzb()),
        password: Some("s3cret".to_owned()),
    };
    let added = http
        .post(&jobs)
        .bearer_auth(CONTROL)
        .json(&job)
        .send()
        .await
        .unwrap();
    assert!(added.status().is_success(), "{}", added.status());
    assert_eq!(added.json::<JobStatus>().await.unwrap().id, job.id);

    let listed = http.get(&jobs).bearer_auth(CONTROL).send().await.unwrap();
    assert!(listed.status().is_success());
    assert_eq!(listed.json::<Vec<JobStatus>>().await.unwrap().len(), 1);

    let status = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let resp = http
                .get(format!("{jobs}/{}", job.id))
                .bearer_auth(CONTROL)
                .send()
                .await
                .unwrap();
            assert!(resp.status().is_success(), "{}", resp.status());
            let status: JobStatus = resp.json().await.unwrap();
            if status.state == JobState::Completed {
                return status;
            }
            assert!((0.0..=1.0).contains(&status.progress), "{status:?}");
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("the job completes");
    let storage = status.storage.expect("storage");
    assert!(std::path::Path::new(&storage).is_absolute(), "{storage}");

    let unknown = http
        .get(format!("{jobs}/never"))
        .bearer_auth(CONTROL)
        .send()
        .await
        .unwrap();
    assert_eq!(
        unknown.status(),
        StatusCode::NOT_FOUND,
        "the api reads 404 as a lost job"
    );

    for expected in [StatusCode::NO_CONTENT, StatusCode::NOT_FOUND] {
        let deleted = http
            .delete(format!("{jobs}/{}", job.id))
            .query(&[("delete_files", true)])
            .bearer_auth(CONTROL)
            .send()
            .await
            .unwrap();
        assert_eq!(deleted.status(), expected);
    }
    assert!(!std::path::Path::new(&storage).exists());

    let imported = harness.import(&season, "idx:contract").await;
    let health = http
        .post(format!(
            "{base}/v1/releases/{}/health",
            imported.release_key
        ))
        .query(&[("percent", 5u8)])
        .bearer_auth(CONTROL)
        .send()
        .await
        .unwrap();
    assert!(health.status().is_success(), "{}", health.status());
    let sample: HealthSample = health.json().await.unwrap();
    assert_eq!(sample.missing, 0);
    assert!(sample.checked > 0);
}
