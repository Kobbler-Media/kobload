use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use http_body_util::BodyExt;
use kob_usenet::vendor::testkit::LoopbackServer;
use kob_usenet::{JobFailure, JobState, JobStatus};
use kobload_usenet::{
    AppState, ConfigPull, Holds, RateWindow, engine_activity, job_activity, job_phase, router,
};
use tower::ServiceExt;
use usenet_proto::{
    EngineActivity, EngineConfigResponse, EngineStatus, IpPreference, JobActivity, JobPhase,
    ProviderActivity, Purpose, StreamActivity, StreamKind, UsenetServer, WaitReason,
};

use crate::pull::{kobload, usenet_server};
use crate::support::{CONTROL, Fixture, Harness, SECRET, collect, engine, json, output, token};

const MB: u64 = 1 << 20;

fn job(state: JobState, done: u64, total: u64) -> JobStatus {
    JobStatus {
        id: "job".to_owned(),
        name: "Release".to_owned(),
        category: "tv".to_owned(),
        state,
        bytes_total: total,
        bytes_done: done,
        segments_total: 10,
        segments_done: 5,
        error: None,
        failure: None,
        path: PathBuf::from("/downloads/tv/Release"),
    }
}

fn activity(phase: JobPhase, waiting: Option<WaitReason>) -> JobActivity {
    JobActivity {
        id: "job".to_owned(),
        phase,
        downloaded_bytes: 1,
        total_bytes: 2,
        speed_bps: None,
        eta_seconds: None,
        waiting,
    }
}

fn provider(host: &str, failing: bool) -> ProviderActivity {
    ProviderActivity {
        host: host.to_owned(),
        in_use: 0,
        open: 0,
        max: 4,
        failing,
    }
}

fn watching(kind: StreamKind) -> StreamActivity {
    StreamActivity {
        id: 1,
        name: "Feature.mkv".to_owned(),
        kind,
        position_percent: Some(10),
        speed_bps: None,
    }
}

async fn status(harness: &Harness) -> EngineStatus {
    let response = harness.control("GET", "/v1/status", None).await;
    assert_eq!(response.status(), StatusCode::OK);
    json(response).await
}

async fn until_no_streams(read: impl AsyncFn() -> EngineStatus) {
    for _ in 0..200 {
        if read().await.streams.is_empty() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the stream entry never went away");
}

#[test]
fn the_speed_window_reads_a_burst_then_a_stall_as_zero() {
    let start = Instant::now();
    let at = |s| start + Duration::from_secs(s);
    let mut window = RateWindow::new(Duration::from_secs(5));
    let key = "provider".to_owned();

    window.record(at(0), [(key.clone(), 0)]);
    assert_eq!(window.rate(&key), None, "one sample measures nothing");
    window.record(at(2), [(key.clone(), 20 * MB)]);
    assert_eq!(window.rate(&key), Some(10 * MB));
    window.record(at(4), [(key.clone(), 20 * MB)]);
    assert_eq!(window.rate(&key), Some(5 * MB), "the burst is averaged");
    window.record(at(7), [(key.clone(), 20 * MB)]);
    window.record(at(9), [(key.clone(), 20 * MB)]);
    assert_eq!(window.rate(&key), Some(0), "a stall reads 0");
}

#[test]
fn the_speed_window_survives_gaps_resets_and_new_keys() {
    let start = Instant::now();
    let at = |s| start + Duration::from_secs(s);
    let mut window = RateWindow::new(Duration::from_secs(5));
    let (a, b) = ("a".to_owned(), "b".to_owned());

    window.record(at(0), [(a.clone(), 100)]);
    window.record(at(60), [(a.clone(), 900)]);
    assert_eq!(window.rate(&a), None, "a stale baseline is not a rate");
    window.record(at(62), [(a.clone(), 100), (b.clone(), 50)]);
    assert_eq!(window.rate(&a), Some(0), "a counter reset is not negative");
    assert_eq!(window.rate(&b), None, "a new key has no baseline");
    window.record(at(64), [(b.clone(), 250)]);
    assert_eq!(window.rate(&a), None, "a key that left has no rate");
    assert_eq!(window.rate(&b), Some(100));
}

#[test]
fn every_job_state_maps_to_its_phase() {
    for (state, phase) in [
        (JobState::Queued, Some(JobPhase::Queued)),
        (JobState::Downloading, Some(JobPhase::Downloading)),
        (JobState::Verifying, Some(JobPhase::Verifying)),
        (JobState::Repairing, Some(JobPhase::Repairing)),
        (JobState::Unpacking, Some(JobPhase::Unpacking)),
        (JobState::Completed, None),
        (JobState::Failed, None),
    ] {
        assert_eq!(job_phase(state), phase, "{state:?}");
    }
}

#[test]
fn a_moving_download_has_a_speed_and_an_eta() {
    let shown = job_activity(
        &job(JobState::Downloading, 40 * MB, 100 * MB),
        Some(10 * MB),
        Holds {
            low_disk: true,
            stream: true,
        },
    )
    .unwrap();
    assert_eq!(shown.phase, JobPhase::Downloading);
    assert_eq!(
        (shown.downloaded_bytes, shown.total_bytes),
        (40 * MB, 100 * MB)
    );
    assert_eq!(shown.speed_bps, Some(10 * MB));
    assert_eq!(shown.eta_seconds, Some(6));
    assert_eq!(shown.waiting, None, "bytes are moving, so nothing holds it");
}

#[test]
fn a_held_job_says_why_and_has_no_eta() {
    for (holds, waiting) in [
        (
            Holds {
                low_disk: false,
                stream: true,
            },
            Some(WaitReason::Stream),
        ),
        (
            Holds {
                low_disk: true,
                stream: false,
            },
            Some(WaitReason::LowDisk),
        ),
        (
            Holds {
                low_disk: true,
                stream: true,
            },
            Some(WaitReason::LowDisk),
        ),
        (Holds::default(), None),
    ] {
        let shown = job_activity(&job(JobState::Downloading, MB, 9 * MB), Some(0), holds).unwrap();
        assert_eq!(shown.waiting, waiting, "{holds:?}");
        assert_eq!(shown.eta_seconds, None, "{holds:?}");
    }
}

#[test]
fn phases_without_moving_bytes_have_no_speed_or_eta_or_wait() {
    let holds = Holds {
        low_disk: true,
        stream: true,
    };
    for state in [
        JobState::Queued,
        JobState::Verifying,
        JobState::Repairing,
        JobState::Unpacking,
    ] {
        let shown = job_activity(&job(state, MB, 9 * MB), Some(5 * MB), holds).unwrap();
        assert_eq!(shown.speed_bps, None, "{state:?}");
        assert_eq!(shown.eta_seconds, None, "{state:?}");
        assert_eq!(shown.waiting, None, "{state:?}");
    }
    let unmeasured = job_activity(&job(JobState::Downloading, MB, 9 * MB), None, holds).unwrap();
    assert_eq!(unmeasured.eta_seconds, None);
    assert_eq!(unmeasured.waiting, None, "unmeasured is not held");
    for state in [JobState::Completed, JobState::Failed] {
        assert_eq!(job_activity(&job(state, 9, 9), None, holds), None);
    }
}

#[test]
fn the_engine_line_picks_the_most_important_state() {
    let ok = [provider("news.ok.test", false)];
    let bad = [
        provider("news.ok.test", false),
        provider("news.bad.test", true),
    ];
    let moving = [activity(JobPhase::Downloading, None)];
    let held = [activity(JobPhase::Downloading, Some(WaitReason::Stream))];
    let disk = [activity(JobPhase::Downloading, Some(WaitReason::LowDisk))];
    let play = [watching(StreamKind::Playing)];
    let failing = EngineActivity::ProviderFailing {
        host: "news.bad.test".to_owned(),
    };

    let cases: [(bool, &[_], &[_], &[_], EngineActivity); 7] = [
        (false, &ok, &[], &[], EngineActivity::Idle),
        (false, &ok, &moving, &[], EngineActivity::Downloading),
        (false, &ok, &[], &play, EngineActivity::Streaming),
        (false, &ok, &moving, &play, EngineActivity::Streaming),
        (
            false,
            &ok,
            &held,
            &play,
            EngineActivity::DownloadsWaitingForStream,
        ),
        (false, &bad, &held, &play, failing),
        (true, &bad, &disk, &play, EngineActivity::PausedLowDisk),
    ];
    for (low_disk, providers, jobs, streams, expected) in cases {
        assert_eq!(
            engine_activity(low_disk, providers, jobs, streams),
            expected
        );
    }
}

#[tokio::test]
async fn a_stream_shows_while_it_runs_and_leaves_when_it_ends() {
    let fixture = Fixture::feature();
    let nntp = LoopbackServer::start(fixture.articles()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let harness = Harness::new(dir.path(), &nntp).await;
    let release = harness.import(&fixture, "idx:feature").await;
    let index = output(&release, "Feature.2026.mkv");

    for (purpose, kind) in [
        (Purpose::Play, StreamKind::Playing),
        (Purpose::Cache, StreamKind::CachingAhead),
        (Purpose::Warm, StreamKind::Preloading),
    ] {
        let response = harness
            .stream("GET", &token(&release.release_key, index, purpose), None)
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        let shown = status(&harness).await;
        assert_eq!(shown.streams.len(), 1, "{purpose:?}");
        assert_eq!(shown.streams[0].name, "Feature.2026.mkv");
        assert_eq!(shown.streams[0].kind, kind);
        assert!(shown.streams[0].position_percent.is_some());

        let (body, error) = collect(response).await;
        assert_eq!((body.len(), error), (fixture.data(0).len(), None));
        assert!(status(&harness).await.streams.is_empty(), "{purpose:?}");
    }
}

#[tokio::test]
async fn a_failed_or_dropped_body_ends_its_entry() {
    let fixture = Fixture::feature();
    let nntp = LoopbackServer::start(fixture.without(0, 40)).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let harness = Harness::new(dir.path(), &nntp).await;
    let release = harness.import(&fixture, "idx:feature").await;
    let index = output(&release, "Feature.2026.mkv");
    let play = token(&release.release_key, index, Purpose::Play);

    let aborted = harness.stream("GET", &play, None).await;
    assert_eq!(status(&harness).await.streams.len(), 1);
    let (_, error) = collect(aborted).await;
    assert!(error.is_some(), "the body aborts at the missing article");
    assert!(status(&harness).await.streams.is_empty());

    let dropped = harness.stream("GET", &play, None).await;
    assert_eq!(status(&harness).await.streams.len(), 1);
    drop(dropped);
    assert!(status(&harness).await.streams.is_empty());
}

#[tokio::test]
async fn overlapping_requests_are_one_entry_until_the_last_ends() {
    let fixture = Fixture::feature();
    let nntp = LoopbackServer::start(fixture.articles()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let harness = Harness::new(dir.path(), &nntp).await;
    let release = harness.import(&fixture, "idx:feature").await;
    let index = output(&release, "Feature.2026.mkv");
    let play = token(&release.release_key, index, Purpose::Play);

    let first = harness.stream("GET", &play, None).await;
    let second = harness.stream("GET", &play, Some("bytes=400000-")).await;
    assert_eq!(status(&harness).await.streams.len(), 1);
    drop(first);
    assert_eq!(status(&harness).await.streams.len(), 1);
    drop(second);
    assert!(status(&harness).await.streams.is_empty());
}

#[tokio::test]
async fn a_client_disconnect_over_a_real_socket_ends_the_entry() {
    let fixture = Fixture::feature();
    let nntp = LoopbackServer::start(fixture.articles()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let harness = Harness::new(dir.path(), &nntp).await;
    let release = harness.import(&fixture, "idx:feature").await;
    let index = output(&release, "Feature.2026.mkv");

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let app = harness.app.clone();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

    let play = token(&release.release_key, index, Purpose::Play);
    let client = reqwest::Client::new();
    let mut response = client
        .get(format!("{url}/stream/{play}"))
        .send()
        .await
        .unwrap();
    response.chunk().await.unwrap().expect("a first chunk");
    assert_eq!(status(&harness).await.streams.len(), 1);
    drop(response);
    drop(client);
    until_no_streams(|| status(&harness)).await;
}

#[tokio::test]
async fn the_status_never_carries_a_secret_and_names_hosts_only() {
    let fixture = Fixture::feature();
    let nntp = LoopbackServer::start(fixture.articles()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let harness = Harness::new(dir.path(), &nntp).await;
    harness.hosts.replace(&[UsenetServer {
        id: "a".to_owned(),
        name: "server a".to_owned(),
        host: "news.host.test".to_owned(),
        port: 563,
        tls: true,
        verify_cert: true,
        ip_preference: IpPreference::Any,
        connections: 4,
        priority: 0,
        backbone: None,
        retention_days: None,
        expires_on: None,
        quota_bytes: None,
        username: "user".to_owned(),
        password: "hunter2-secret".to_owned(),
    }]);
    let release = harness.import(&fixture, "idx:feature").await;
    let index = output(&release, "Feature.2026.mkv");
    let play = token(&release.release_key, index, Purpose::Play);
    let response = harness.stream("GET", &play, None).await;

    let reply = harness.control("GET", "/v1/status", None).await;
    let text = String::from_utf8(
        reply
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap();
    assert!(text.contains("news.host.test"), "{text}");
    assert!(text.contains("Feature.2026.mkv"), "{text}");
    for secret in [
        play.as_str(),
        "/stream/",
        "hunter2-secret",
        "\"user\"",
        "server a",
        CONTROL,
        std::str::from_utf8(SECRET).unwrap(),
        "username",
        "password",
    ] {
        assert!(!text.contains(secret), "{secret} in {text}");
    }
    drop(response);
}

#[tokio::test]
async fn adopted_and_pulled_config_supplies_the_hosts() {
    let nntp = LoopbackServer::start([]).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let engine = engine(dir.path(), vec![crate::support::server("a", &nntp)]).await;
    let state = AppState::new(engine.clone(), CONTROL, SECRET);
    let pulled = usenet_server("a", &nntp, 0);
    let served = Arc::new(Mutex::new(EngineConfigResponse {
        servers: vec![pulled.clone()],
    }));
    let url = kobload(served).await;
    let mut pull = ConfigPull::new(engine, &url, CONTROL, dir.path(), state.configured())
        .with_hosts(state.provider_hosts());
    let app = router(state);
    let host = async || {
        let request = Request::get("/v1/status")
            .header(header::AUTHORIZATION, format!("Bearer {CONTROL}"))
            .body(Body::empty())
            .unwrap();
        let reply = app.clone().oneshot(request).await.unwrap();
        json::<EngineStatus>(reply).await.providers[0].host.clone()
    };

    pull.adopt(vec![UsenetServer {
        host: "news.adopted.test".to_owned(),
        ..pulled.clone()
    }]);
    assert_eq!(host().await, "news.adopted.test");
    pull.tick().await.unwrap();
    assert_eq!(host().await, pulled.host);
}

#[test]
fn a_failed_job_is_not_listed_even_with_a_failure_set() {
    let mut failed = job(JobState::Failed, 1, 2);
    failed.failure = Some(JobFailure::Content);
    failed.error = Some("secret-free".to_owned());
    assert_eq!(job_activity(&failed, Some(1), Holds::default()), None);
}
