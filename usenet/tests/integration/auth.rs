use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use kob_usenet::vendor::testkit::LoopbackServer;

use kobload_usenet::{AppState, router};
use tower::ServiceExt;

use crate::support::{CONTROL, Fixture, Harness, METRICS, SECRET, engine, server};

#[tokio::test]
async fn control_routes_need_the_bearer() {
    let fixture = Fixture::season();
    let nntp = LoopbackServer::start(fixture.articles()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let harness = Harness::new(dir.path(), &nntp).await;
    let routes = [
        ("POST", "/v1/import".to_owned()),
        ("GET", format!("/v1/releases/nzb-{}", "0".repeat(64))),
    ];

    for (method, uri) in &routes {
        for authorization in [None, Some("Bearer wrong"), Some("control-key")] {
            let mut request = Request::builder().method(*method).uri(uri);
            if let Some(value) = authorization {
                request = request.header(header::AUTHORIZATION, value);
            }
            let response = harness.send(request.body(Body::empty()).unwrap()).await;
            assert_eq!(
                response.status(),
                StatusCode::UNAUTHORIZED,
                "{method} {uri} {authorization:?}"
            );
        }
    }

    let health = harness
        .send(Request::get("/healthz").body(Body::empty()).unwrap())
        .await;
    assert_eq!(health.status(), StatusCode::OK);
}

#[tokio::test]
async fn metrics_take_their_own_key_and_not_the_control_key() {
    let fixture = Fixture::season();
    let nntp = LoopbackServer::start(fixture.articles()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let harness = Harness::new(dir.path(), &nntp).await;

    for (authorization, status) in [
        (None, StatusCode::UNAUTHORIZED),
        (Some(format!("Bearer {CONTROL}")), StatusCode::UNAUTHORIZED),
        (
            Some("Bearer metrics-ke".to_owned()),
            StatusCode::UNAUTHORIZED,
        ),
        (Some(format!("Bearer {METRICS}")), StatusCode::OK),
    ] {
        let mut request = Request::get("/metrics");
        if let Some(value) = &authorization {
            request = request.header(header::AUTHORIZATION, value);
        }
        let response = harness.send(request.body(Body::empty()).unwrap()).await;
        assert_eq!(response.status(), status, "{authorization:?}");
    }
}

#[tokio::test]
async fn without_a_metrics_key_metrics_stay_closed() {
    let nntp = LoopbackServer::start([]).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let engine = engine(dir.path(), vec![server("a", &nntp)]).await;
    let app = router(AppState::new(engine, CONTROL, SECRET));
    for key in [CONTROL, METRICS] {
        let request = Request::get("/metrics")
            .header(header::AUTHORIZATION, format!("Bearer {key}"))
            .body(Body::empty())
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{key}");
    }
}

#[tokio::test]
async fn health_is_unavailable_until_a_config_is_applied() {
    let nntp = LoopbackServer::start([]).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let engine = engine(dir.path(), vec![server("a", &nntp)]).await;
    let state = AppState::new(engine, CONTROL, SECRET);
    let configured = state.configured();
    let app = router(state);
    let health = || async {
        app.clone()
            .oneshot(Request::get("/healthz").body(Body::empty()).unwrap())
            .await
            .unwrap()
            .status()
    };
    assert_eq!(health().await, StatusCode::SERVICE_UNAVAILABLE);
    configured.set();
    assert_eq!(health().await, StatusCode::OK);
}
