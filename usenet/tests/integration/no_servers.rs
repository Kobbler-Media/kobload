//! kobload leaves a server out of the config when SAB fills its account's cap, and that can
//! be every server. A job caught by it waits out its retry budget and never finishes damaged.

use std::time::Duration;

use kob_usenet::vendor::testkit::LoopbackServer;
use usenet_proto::{JobFailure, JobState};

use crate::support::{Fixture, Harness, quick_retries, server};

const OUTAGE: Duration = Duration::from_millis(600);

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_job_queued_with_no_servers_waits_and_finishes_when_one_returns() {
    let fixture = Fixture::feature();
    let nntp = LoopbackServer::start(fixture.articles()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let harness = Harness::with_download(dir.path(), &nntp, quick_retries(1000)).await;
    harness.engine.reconfigure(Vec::new()).unwrap();

    harness.add_job("empty", &fixture).await;
    tokio::time::sleep(OUTAGE).await;
    let waiting = harness.job("empty").await;
    assert_ne!(waiting.state, JobState::Failed, "{waiting:?}");
    assert_ne!(waiting.state, JobState::Completed, "{waiting:?}");

    harness
        .engine
        .reconfigure(vec![server("a", &nntp)])
        .unwrap();
    let done = harness.finished("empty").await;
    assert_eq!(done.state, JobState::Completed, "{done:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_job_losing_every_server_midway_finishes_intact_when_one_returns() {
    let fixture = Fixture::feature();
    let nntp = LoopbackServer::start(fixture.articles()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let harness = Harness::with_download(dir.path(), &nntp, quick_retries(1000)).await;
    let (arrived, release) = nntp.hold_article(fixture.message_id(0, 30));

    harness.add_job("midway", &fixture).await;
    arrived.await.unwrap();
    harness.engine.reconfigure(Vec::new()).unwrap();
    drop(release);
    tokio::time::sleep(OUTAGE).await;
    let waiting = harness.job("midway").await;
    assert_ne!(waiting.state, JobState::Failed, "{waiting:?}");

    harness
        .engine
        .reconfigure(vec![server("a", &nntp)])
        .unwrap();
    let done = harness.finished("midway").await;
    assert_eq!(done.state, JobState::Completed, "{done:?}");
    let storage = std::path::PathBuf::from(done.storage.expect("storage"));
    let file = storage.join("Feature.2026.mkv");
    assert!(
        std::fs::read(&file).unwrap() == fixture.data(0),
        "bytes intact"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_outage_past_the_retry_budget_fails_as_infrastructure_not_content() {
    let fixture = Fixture::feature();
    let nntp = LoopbackServer::start(fixture.articles()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let harness = Harness::with_download(dir.path(), &nntp, quick_retries(2)).await;
    harness.engine.reconfigure(Vec::new()).unwrap();

    harness.add_job("spent", &fixture).await;
    let done = harness.finished("spent").await;
    assert_eq!(done.state, JobState::Failed, "{done:?}");
    assert_eq!(done.failure, Some(JobFailure::Infrastructure), "{done:?}");
}
