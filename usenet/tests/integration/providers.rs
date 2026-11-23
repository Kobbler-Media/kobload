use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use kob_usenet::{IpPreference, ServerConfig};
use kobload_usenet::{AppState, router};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;
use tower::ServiceExt;

use crate::support::{CONTROL, Fixture, METRICS, SECRET, collect, engine, import_body, metric};

async fn refusing_nntp() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        while let Ok((socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let (read, mut write) = socket.into_split();
                let mut lines = BufReader::new(read).lines();
                write.write_all(b"200 fake news ready\r\n").await.ok();
                while let Ok(Some(line)) = lines.next_line().await {
                    let reply: &[u8] = if line.starts_with("AUTHINFO USER") {
                        b"381 password required\r\n"
                    } else if line.starts_with("AUTHINFO PASS") {
                        b"481 authentication failed\r\n"
                    } else {
                        b"480 authentication required\r\n"
                    };
                    write.write_all(reply).await.ok();
                }
            });
        }
    });
    port
}

fn refused_server(port: u16) -> ServerConfig {
    ServerConfig {
        id: "a".to_owned(),
        name: "server a".to_owned(),
        host: "127.0.0.1".to_owned(),
        port,
        tls: false,
        verify_cert: true,
        ip_preference: IpPreference::Any,
        connections: 2,
        priority: 0,
        backbone: None,
        retention_days: None,
        expires_on: None,
        quota_bytes: None,
        username: "user".to_owned(),
        password: "wrong-password".to_owned(),
    }
}

#[tokio::test]
async fn a_provider_that_refuses_the_login_shows_as_failing_in_metrics() {
    let port = refusing_nntp().await;
    let dir = tempfile::tempdir().unwrap();
    let engine = engine(dir.path(), vec![refused_server(port)]).await;
    let state = AppState::new(Arc::clone(&engine), CONTROL, SECRET).with_metrics_key(Some(METRICS));
    state.configured().set();
    let app = router(state);

    let import = Request::post("/v1/import")
        .header(header::AUTHORIZATION, format!("Bearer {CONTROL}"))
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(
            import_body(Some(&Fixture::season()), "idx:refused").to_string(),
        ))
        .unwrap();
    let response = app.clone().oneshot(import).await.unwrap();
    assert!(!response.status().is_success(), "{}", response.status());

    let scrape = Request::get("/metrics")
        .header(header::AUTHORIZATION, format!("Bearer {METRICS}"))
        .body(Body::empty())
        .unwrap();
    let response = app.oneshot(scrape).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let text = String::from_utf8(collect(response).await.0).unwrap();
    let provider = "provider=\"server a\"";
    assert!(
        metric(&text, "kobload_usenet_connect_failures_total", &[provider]) >= 1,
        "{text}"
    );
    assert!(
        metric(&text, "kobload_usenet_provider_failing", &[provider]) == 1,
        "{text}"
    );
}
