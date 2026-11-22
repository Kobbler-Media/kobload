use std::sync::{Arc, Mutex};

use axum::Router;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use kob_usenet::vendor::testkit::LoopbackServer;
use kobload_usenet::{ConfigPull, Configured, Pulled};
use usenet_proto::{EngineConfigResponse, IpPreference, UsenetServer};

use crate::support::{CONTROL, engine};

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
