use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, Request, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use kob_usenet::vendor::testkit::LoopbackServer;
use kobload_usenet::{AppState, ConfigPull, Configured, Pulled, router};
use tower::ServiceExt;
use usenet_proto::{EngineConfigResponse, IpPreference, UsenetServer};

use crate::support::{CONTROL, SECRET, engine};

type Served = Arc<Mutex<EngineConfigResponse>>;

async fn engine_config(State(served): State<Served>, headers: HeaderMap) -> Response {
    if headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        != Some(&format!("Bearer {CONTROL}"))
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    axum::Json(served.lock().unwrap().clone()).into_response()
}

async fn kobload(served: Served) -> String {
    let app = Router::new()
        .route("/api/v1/usenet/engine-config", get(engine_config))
        .with_state(served);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    url
}

fn usenet_server(id: &str, nntp: &LoopbackServer, priority: u32) -> UsenetServer {
    UsenetServer {
        id: id.to_owned(),
        name: format!("provider {id}"),
        host: "127.0.0.1".to_owned(),
        port: nntp.addr().port(),
        tls: false,
        verify_cert: true,
        ip_preference: IpPreference::V4,
        connections: 2,
        priority,
        backbone: Some("bb".to_owned()),
        retention_days: Some(4000),
        expires_on: None,
        quota_bytes: None,
        username: "user".to_owned(),
        password: "hunter2-secret".to_owned(),
    }
}

fn names(engine: &kob_usenet::Engine) -> Vec<(String, u32, usize)> {
    let mut names: Vec<_> = engine
        .stats()
        .into_iter()
        .map(|p| (p.name, p.priority, p.max_connections))
        .collect();
    names.sort();
    names
}

#[tokio::test]
async fn the_engine_applies_pulled_servers_only_when_they_change() {
    let nntp = LoopbackServer::start([]).await.unwrap();
    let served: Served = Arc::new(Mutex::new(EngineConfigResponse {
        servers: vec![usenet_server("a", &nntp, 0)],
    }));
    let url = kobload(Arc::clone(&served)).await;
    let dir = tempfile::tempdir().unwrap();
    let engine = engine(dir.path(), Vec::new()).await;
    assert!(names(&engine).is_empty());
    let mut pull = ConfigPull::new(
        Arc::clone(&engine),
        &url,
        CONTROL,
        dir.path(),
        Configured::default(),
    );

    assert_eq!(pull.tick().await.unwrap(), Pulled::Applied(1));
    assert_eq!(names(&engine), [("provider a".to_owned(), 0, 2)]);
    assert_eq!(pull.tick().await.unwrap(), Pulled::Unchanged);

    served
        .lock()
        .unwrap()
        .servers
        .push(usenet_server("b", &nntp, 99));
    assert_eq!(pull.tick().await.unwrap(), Pulled::Applied(2));
    assert_eq!(
        names(&engine),
        [
            ("provider a".to_owned(), 0, 2),
            ("provider b".to_owned(), 99, 2)
        ]
    );
    assert_eq!(pull.tick().await.unwrap(), Pulled::Unchanged);
}

#[tokio::test]
async fn a_server_left_out_of_the_config_goes_and_an_empty_config_survives_a_restart() {
    let nntp = LoopbackServer::start([]).await.unwrap();
    let served: Served = Arc::new(Mutex::new(EngineConfigResponse {
        servers: vec![usenet_server("a", &nntp, 0), usenet_server("b", &nntp, 1)],
    }));
    let url = kobload(Arc::clone(&served)).await;
    let dir = tempfile::tempdir().unwrap();
    let engine = engine(dir.path(), Vec::new()).await;
    let mut pull = ConfigPull::new(
        Arc::clone(&engine),
        &url,
        CONTROL,
        dir.path(),
        Configured::default(),
    );
    assert_eq!(pull.tick().await.unwrap(), Pulled::Applied(2));

    served.lock().unwrap().servers.remove(0);
    assert_eq!(pull.tick().await.unwrap(), Pulled::Applied(1));
    assert_eq!(names(&engine), [("provider b".to_owned(), 1, 2)]);

    served.lock().unwrap().servers.clear();
    assert_eq!(pull.tick().await.unwrap(), Pulled::Applied(0));
    assert!(names(&engine).is_empty());
    assert_eq!(pull.tick().await.unwrap(), Pulled::Unchanged);
    let restarted = tempfile::tempdir().unwrap();
    std::fs::copy(
        dir.path().join("config.json"),
        restarted.path().join("config.json"),
    )
    .unwrap();
    let booted = kobload_usenet::start_engine(kob_usenet::EngineConfig::new(
        restarted.path().to_path_buf(),
        Vec::new(),
    ))
    .await
    .unwrap();
    assert_eq!(booted.restored, Some(Vec::new()));
    assert!(names(&booted.engine).is_empty());

    served.lock().unwrap().servers = vec![usenet_server("a", &nntp, 0)];
    assert_eq!(pull.tick().await.unwrap(), Pulled::Applied(1));
    assert_eq!(names(&engine), [("provider a".to_owned(), 0, 2)]);
}

#[tokio::test]
async fn a_pull_with_the_wrong_key_applies_nothing() {
    let nntp = LoopbackServer::start([]).await.unwrap();
    let served: Served = Arc::new(Mutex::new(EngineConfigResponse {
        servers: vec![usenet_server("a", &nntp, 0)],
    }));
    let url = kobload(served).await;
    let dir = tempfile::tempdir().unwrap();
    let engine = engine(dir.path(), Vec::new()).await;
    let mut pull = ConfigPull::new(
        Arc::clone(&engine),
        &url,
        "wrong-key",
        dir.path(),
        Configured::default(),
    );

    let error = pull.tick().await.unwrap_err();
    assert!(error.to_string().contains("401"), "{error}");
    assert!(names(&engine).is_empty());
}

#[tokio::test]
async fn a_leftover_partial_file_cannot_leave_the_stored_config_readable() {
    use std::os::unix::fs::PermissionsExt as _;
    let nntp = LoopbackServer::start([]).await.unwrap();
    let served: Served = Arc::new(Mutex::new(EngineConfigResponse {
        servers: vec![usenet_server("a", &nntp, 0)],
    }));
    let url = kobload(served).await;
    let dir = tempfile::tempdir().unwrap();
    let partial = dir.path().join("config.json.partial");
    std::fs::write(&partial, b"{}").unwrap();
    std::fs::set_permissions(&partial, std::fs::Permissions::from_mode(0o644)).unwrap();
    let engine = engine(dir.path(), Vec::new()).await;
    let mut pull = ConfigPull::new(engine, &url, CONTROL, dir.path(), Configured::default());

    assert_eq!(pull.tick().await.unwrap(), Pulled::Applied(1));
    let mode = std::fs::metadata(dir.path().join("config.json"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o777, 0o600, "{mode:o}");
}

#[tokio::test]
async fn a_config_that_cannot_be_stored_still_serves_and_is_not_reapplied() {
    let nntp = LoopbackServer::start([]).await.unwrap();
    let served: Served = Arc::new(Mutex::new(EngineConfigResponse {
        servers: vec![usenet_server("a", &nntp, 0)],
    }));
    let url = kobload(served).await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("config.json.partial")).unwrap();
    let engine = engine(dir.path(), Vec::new()).await;
    let configured = Configured::default();
    let mut pull = ConfigPull::new(
        Arc::clone(&engine),
        &url,
        CONTROL,
        dir.path(),
        configured.clone(),
    );

    assert_eq!(pull.tick().await.unwrap(), Pulled::Applied(1));
    assert!(configured.is_set());
    assert_eq!(names(&engine), [("provider a".to_owned(), 0, 2)]);
    assert_eq!(pull.tick().await.unwrap(), Pulled::Unchanged);
}

#[tokio::test]
async fn a_refresh_request_pulls_at_once_instead_of_at_the_next_tick() {
    let nntp = LoopbackServer::start([]).await.unwrap();
    let served: Served = Arc::new(Mutex::new(EngineConfigResponse {
        servers: vec![usenet_server("a", &nntp, 0)],
    }));
    let url = kobload(Arc::clone(&served)).await;
    let dir = tempfile::tempdir().unwrap();
    let engine = engine(dir.path(), Vec::new()).await;
    let state = AppState::new(Arc::clone(&engine), CONTROL, SECRET);
    let refresh = state.config_refresh();
    let app = router(state);
    let pull = ConfigPull::new(
        Arc::clone(&engine),
        &url,
        CONTROL,
        dir.path(),
        Configured::default(),
    )
    .with_refresh(refresh);
    tokio::spawn(pull.run(Duration::from_secs(3600)));
    let until = |want: usize| {
        let engine = Arc::clone(&engine);
        async move {
            tokio::time::timeout(Duration::from_secs(3), async {
                while names(&engine).first().map(|n| n.2) != Some(want) {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            })
            .await
            .expect("the new config applied");
        }
    };
    until(2).await;

    served.lock().unwrap().servers[0].connections = 1;
    let response = app
        .oneshot(
            Request::post("/v1/config/refresh")
                .header(header::AUTHORIZATION, format!("Bearer {CONTROL}"))
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
    until(1).await;
}
