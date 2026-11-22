use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use kob_usenet::vendor::testkit::LoopbackServer;

use crate::support::{Fixture, Harness};

#[tokio::test]
async fn control_routes_need_the_bearer() {
    let fixture = Fixture::season();
    let nntp = LoopbackServer::start(fixture.articles()).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let harness = Harness::new(dir.path(), &nntp).await;
    let routes = [
        ("POST", "/v1/import".to_owned()),
        ("GET", format!("/v1/releases/nzb-{}", "0".repeat(64))),
        ("GET", "/metrics".to_owned()),
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
