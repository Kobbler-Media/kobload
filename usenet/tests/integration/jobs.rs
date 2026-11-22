use std::path::{Path, PathBuf};
use std::time::Duration;

use axum::http::StatusCode;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use kob_usenet::vendor::testkit::LoopbackServer;
use usenet_proto::{JobFailure, JobRequest, JobState, JobStatus};

use crate::support::{Fixture, Harness, json, metric, quick_retries};

const FINISH: Duration = Duration::from_secs(30);

pub fn job_request(id: &str, fixture: &Fixture) -> serde_json::Value {
    serde_json::to_value(JobRequest {
        id: id.to_owned(),
        name: "Feature.2026.1080p".to_owned(),
        category: "kobload".to_owned(),
        nzb_base64: STANDARD.encode(fixture.nzb()),
        password: None,
    })
    .unwrap()
}

impl Harness {
    pub async fn add_job(&self, id: &str, fixture: &Fixture) -> JobStatus {
        let response = self
            .control("POST", "/v1/jobs", Some(job_request(id, fixture)))
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        json(response).await
    }

    pub async fn job(&self, id: &str) -> JobStatus {
        let response = self.control("GET", &format!("/v1/jobs/{id}"), None).await;
        assert_eq!(response.status(), StatusCode::OK, "job {id}");
        json(response).await
    }

    pub async fn settled(&self, id: &str, done: impl Fn(&JobStatus) -> bool) -> JobStatus {
        tokio::time::timeout(FINISH, async {
            loop {
                let status = self.job(id).await;
                if done(&status) {
                    return status;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("job {id} never settled"))
    }

    pub async fn finished(&self, id: &str) -> JobStatus {
        self.settled(id, |s| {
            matches!(s.state, JobState::Completed | JobState::Failed)
        })
        .await
    }
}

fn found(dir: &Path, name: &str) -> Option<PathBuf> {
    std::fs::read_dir(dir).ok()?.flatten().find_map(|entry| {
        let path = entry.path();
        if path.is_dir() {
            found(&path, name)
        } else {
            (entry.file_name() == name).then_some(path)
        }
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_job_reports_fractional_progress_and_completes_into_the_category_folder() {
    let fixture = Fixture::feature();
    let nntp = LoopbackServer::start(fixture.articles()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let harness = Harness::new(dir.path(), &nntp).await;
    let (arrived, release) = nntp.hold_article(fixture.message_id(0, 30));

    let added = harness.add_job("dl-1", &fixture).await;
    assert_eq!(added.id, "dl-1");
    assert_eq!(added.name, "Feature.2026.1080p");
    assert_eq!(added.state, JobState::Queued);
    assert_eq!(added.storage, None);
    assert_eq!(added.failure, None);

    arrived.await.unwrap();
    let midway = harness
        .settled("dl-1", |s| s.downloaded_bytes > 0 && s.total_bytes > 0)
        .await;
    assert_eq!(midway.state, JobState::Downloading);
    assert!(
        midway.progress > 0.0 && midway.progress < 1.0,
        "progress is a fraction: {midway:?}"
    );
    assert_eq!(midway.storage, None, "no storage until the job completes");
    drop(release);

    let done = harness.finished("dl-1").await;
    assert_eq!(done.state, JobState::Completed, "{done:?}");
    assert!((done.progress - 1.0).abs() < f64::EPSILON);
    assert_eq!(done.downloaded_bytes, done.total_bytes);
    let storage = PathBuf::from(done.storage.expect("storage"));
    assert!(
        storage.starts_with(harness.downloads.join("kobload")),
        "{} is under the category folder",
        storage.display()
    );
    let file = found(&storage, "Feature.2026.mkv").expect("the finished file");
    assert_eq!(std::fs::read(file).unwrap(), fixture.data(0));

    let listed: Vec<JobStatus> = json(harness.control("GET", "/v1/jobs", None).await).await;
    assert_eq!(
        listed.iter().map(|j| j.id.as_str()).collect::<Vec<_>>(),
        ["dl-1"]
    );
    let metrics = harness.metrics().await;
    assert_eq!(
        metric(&metrics, "kobload_usenet_jobs", &["state=\"completed\""]),
        1
    );
    assert_eq!(
        metric(&metrics, "kobload_usenet_jobs", &["state=\"downloading\""]),
        0
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn deleting_a_job_keeps_or_removes_its_files() {
    let fixture = Fixture::feature();
    let nntp = LoopbackServer::start(fixture.articles()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let harness = Harness::new(dir.path(), &nntp).await;

    for (id, delete_files, kept) in [("keep", "false", true), ("drop", "true", false)] {
        harness.add_job(id, &fixture).await;
        let done = harness.finished(id).await;
        assert_eq!(done.state, JobState::Completed, "{done:?}");
        let storage = PathBuf::from(done.storage.unwrap());

        let deleted = harness
            .control(
                "DELETE",
                &format!("/v1/jobs/{id}?delete_files={delete_files}"),
                None,
            )
            .await;
        assert_eq!(deleted.status(), StatusCode::NO_CONTENT, "{id}");
        let gone = harness
            .control("GET", &format!("/v1/jobs/{id}"), None)
            .await;
        assert_eq!(gone.status(), StatusCode::NOT_FOUND, "{id}");
        assert_eq!(storage.exists(), kept, "{id}: {}", storage.display());
    }

    let unknown = harness.control("DELETE", "/v1/jobs/never", None).await;
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn job_failures_carry_their_kind_and_a_queued_retry_carries_none() {
    struct Case {
        name: &'static str,
        articles_missing: bool,
        providers_down: bool,
        retry_attempts: u32,
        state: JobState,
        failure: Option<JobFailure>,
    }
    let cases = [
        Case {
            name: "articles missing beyond repair",
            articles_missing: true,
            providers_down: false,
            retry_attempts: 12,
            state: JobState::Failed,
            failure: Some(JobFailure::Content),
        },
        Case {
            name: "providers down on the last try",
            articles_missing: false,
            providers_down: true,
            retry_attempts: 1,
            state: JobState::Failed,
            failure: Some(JobFailure::Infrastructure),
        },
        Case {
            name: "providers down with retries left",
            articles_missing: false,
            providers_down: true,
            retry_attempts: 12,
            state: JobState::Queued,
            failure: None,
        },
    ];
    for case in cases {
        let fixture = Fixture::feature();
        let articles = if case.articles_missing {
            fixture.without(0, 20)
        } else {
            fixture.articles()
        };
        let nntp = LoopbackServer::start(articles).await.unwrap();
        if case.providers_down {
            nntp.refuse().await;
        }
        let dir = tempfile::tempdir().unwrap();
        let harness =
            Harness::with_download(dir.path(), &nntp, quick_retries(case.retry_attempts)).await;
        harness.add_job("dl", &fixture).await;

        let status = harness
            .settled("dl", |s| s.state == case.state && s.error.is_some())
            .await;
        assert_eq!(status.failure, case.failure, "{}: {status:?}", case.name);
        assert_eq!(status.storage, None, "{}", case.name);
    }
}

#[tokio::test]
async fn a_job_request_is_checked_before_it_is_queued() {
    let fixture = Fixture::feature();
    let nntp = LoopbackServer::start(fixture.articles()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let harness = Harness::new(dir.path(), &nntp).await;

    for category in ["", "..", "a/b", "../escape", "."] {
        let mut body = job_request("bad-category", &fixture);
        body["category"] = category.into();
        let response = harness.control("POST", "/v1/jobs", Some(body)).await;
        assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{category:?}");
    }
    let mut body = job_request("bad-nzb", &fixture);
    body["nzb_base64"] = "!!!".into();
    let response = harness.control("POST", "/v1/jobs", Some(body)).await;
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);

    harness.add_job("twice", &fixture).await;
    let again = harness
        .control("POST", "/v1/jobs", Some(job_request("twice", &fixture)))
        .await;
    assert_eq!(again.status(), StatusCode::CONFLICT);

    let unknown = harness.control("GET", "/v1/jobs/never", None).await;
    assert_eq!(unknown.status(), StatusCode::NOT_FOUND);
}
