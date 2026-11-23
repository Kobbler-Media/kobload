use std::os::unix::fs::PermissionsExt as _;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use axum::Router;
use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use kob_usenet::EngineConfig;
use kob_usenet::vendor::testkit::LoopbackServer;
use kobload_usenet::{Booted, ConfigPull, Configured, Pulled, start_engine};
use usenet_proto::{EngineConfigResponse, IpPreference, UsenetServer};

use crate::support::{CONTROL, engine, warm_engine};

struct Flaky {
    config: EngineConfigResponse,
    refuse: usize,
    calls: AtomicUsize,
}

async fn engine_config(State(flaky): State<Arc<Flaky>>) -> Response {
    if flaky.calls.fetch_add(1, Ordering::SeqCst) < flaky.refuse {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    axum::Json(flaky.config.clone()).into_response()
}

async fn kobload(nntp: &LoopbackServer, refuse: usize) -> (String, Arc<Flaky>) {
    let flaky = Arc::new(Flaky {
        config: EngineConfigResponse {
            servers: vec![provider(nntp)],
        },
        refuse,
        calls: AtomicUsize::new(0),
    });
    let app = Router::new()
        .route("/api/v1/usenet/engine-config", get(engine_config))
        .with_state(Arc::clone(&flaky));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (url, flaky)
}

fn provider(nntp: &LoopbackServer) -> UsenetServer {
    UsenetServer {
        id: "a".to_owned(),
        name: "provider a".to_owned(),
        host: "127.0.0.1".to_owned(),
        port: nntp.addr().port(),
        tls: false,
        verify_cert: true,
        ip_preference: IpPreference::V4,
        connections: 2,
        priority: 0,
        backbone: None,
        retention_days: None,
        expires_on: None,
        quota_bytes: None,
        username: "user".to_owned(),
        password: "hunter2-secret".to_owned(),
    }
}

fn providers(engine: &kob_usenet::Engine) -> Vec<String> {
    engine.stats().into_iter().map(|p| p.name).collect()
}

#[tokio::test]
async fn the_first_pull_retries_until_kobload_answers() {
    let nntp = LoopbackServer::start([]).await.unwrap();
    let (url, flaky) = kobload(&nntp, 2).await;
    let dir = tempfile::tempdir().unwrap();
    let engine = engine(dir.path(), Vec::new()).await;
    let configured = Configured::default();
    let mut pull = ConfigPull::new(
        Arc::clone(&engine),
        &url,
        CONTROL,
        dir.path(),
        configured.clone(),
    );

    let pulled = tokio::time::timeout(Duration::from_secs(8), pull.first())
        .await
        .expect("retried within the backoff");
    assert_eq!(pulled, Pulled::Applied(1));
    assert_eq!(flaky.calls.load(Ordering::SeqCst), 3);
    assert!(configured.is_set());
    assert_eq!(providers(&engine), ["provider a"]);
}

#[tokio::test]
async fn a_restart_serves_the_last_applied_config_without_kobload() {
    let nntp = LoopbackServer::start([]).await.unwrap();
    let (url, _) = kobload(&nntp, 0).await;
    let dir = tempfile::tempdir().unwrap();
    let first = engine(dir.path(), Vec::new()).await;
    let mut pull = ConfigPull::new(first, &url, CONTROL, dir.path(), Configured::default());
    assert_eq!(pull.tick().await.unwrap(), Pulled::Applied(1));

    let stored = dir.path().join("config.json");
    let mode = std::fs::metadata(&stored).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "the stored config holds passwords");

    let Booted { engine, restored } =
        start_engine(EngineConfig::new(dir.path().to_path_buf(), Vec::new()))
            .await
            .unwrap();
    assert_eq!(providers(&engine), ["provider a"]);
    let configured = Configured::default();
    let mut pull = ConfigPull::new(
        engine,
        "http://127.0.0.1:9",
        CONTROL,
        dir.path(),
        configured.clone(),
    );
    pull.adopt(restored.expect("a stored config"));
    assert!(configured.is_set());
}

#[tokio::test]
async fn a_fresh_data_dir_restores_nothing() {
    let dir = tempfile::tempdir().unwrap();
    let Booted { engine, restored } =
        start_engine(EngineConfig::new(dir.path().to_path_buf(), Vec::new()))
            .await
            .unwrap();
    assert!(restored.is_none());
    assert!(providers(&engine).is_empty());
}

#[tokio::test]
async fn an_applied_config_warms_the_pool() {
    let nntp = LoopbackServer::start([]).await.unwrap();
    let (url, _) = kobload(&nntp, 0).await;
    let dir = tempfile::tempdir().unwrap();
    let engine = warm_engine(dir.path(), Vec::new(), 2).await;
    let mut pull = ConfigPull::new(
        Arc::clone(&engine),
        &url,
        CONTROL,
        dir.path(),
        Configured::default(),
    );
    pull.tick().await.unwrap();

    let live = || engine.stats().iter().map(|p| p.live).sum::<usize>();
    tokio::time::timeout(Duration::from_secs(5), async {
        while live() == 0 {
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .expect("connections opened right after the config applied");
}
