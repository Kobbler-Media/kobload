//! The Downloads page's live usenet stats: an admin-only proxy that keeps answering when the
//! engine is missing or down, and forwards only the typed status.

use reqwest::StatusCode;
use serde_json::{Value, json};

use crate::common::TestClient;
use crate::common::usenet::CONTROL_KEY;
use crate::common::usenet_jobs::EngineFixture;

const STATUS: &str = "/api/v1/admin/usenet/status";

async fn admin_status(fx: &EngineFixture) -> Value {
    let resp = fx.server.admin_client().get(STATUS).await;
    assert_eq!(resp.status, StatusCode::OK, "{}", resp.text());
    serde_json::from_str(&resp.text()).unwrap()
}

fn engine_reply() -> Value {
    json!({
        "activity": { "kind": "provider_failing", "host": "news.host.test" },
        "speed_bps": 1048576,
        "password": "hunter2-leaked",
        "providers": [{
            "host": "news.host.test", "in_use": 3, "open": 8, "max": 20, "failing": true,
            "username": "alice-leaked"
        }],
        "jobs": [{
            "id": "job-1", "phase": "downloading", "downloaded_bytes": 10, "total_bytes": 20,
            "speed_bps": 0, "eta_seconds": null, "waiting": "stream",
            "nzb_url": "http://indexer.test/nzb?apikey=leaked"
        }],
        "streams": [{
            "id": 1, "name": "Feature.mkv", "kind": "caching_ahead", "position_percent": 42,
            "speed_bps": 2048, "url": "http://engine.test/stream/token-leaked"
        }]
    })
}

#[tokio::test]
async fn the_proxy_needs_an_admin_and_refuses_the_engine_key() {
    let fx = EngineFixture::start().await;
    let client = || TestClient::new(&fx.server.api_url);
    assert_eq!(client().get(STATUS).await.status, StatusCode::UNAUTHORIZED);
    for token in [CONTROL_KEY, "wrong"] {
        let resp = client().with_auth(token).get(STATUS).await;
        assert_eq!(resp.status, StatusCode::UNAUTHORIZED, "{token}");
    }
    fx.cleanup().await;
}

#[tokio::test]
async fn the_proxy_forwards_the_typed_status_and_drops_anything_else() {
    let fx = EngineFixture::start().await;
    *fx.engine.status_reply.lock().unwrap() = engine_reply();

    let body = admin_status(&fx).await;

    assert_eq!(body["engine"], "available");
    let status = &body["status"];
    assert_eq!(status["activity"]["kind"], "provider_failing");
    assert_eq!(status["activity"]["host"], "news.host.test");
    assert_eq!(status["providers"][0]["in_use"], 3);
    assert_eq!(status["jobs"][0]["waiting"], "stream");
    assert_eq!(status["streams"][0]["position_percent"], 42);
    let text = body.to_string();
    for secret in [
        "leaked", "password", "username", "nzb_url", "apikey", "token",
    ] {
        assert!(!text.contains(secret), "{secret} in {text}");
    }
    fx.cleanup().await;
}

#[tokio::test]
async fn an_engine_that_is_down_or_refuses_reads_as_unavailable() {
    let down = EngineFixture::unreachable().await;
    assert_eq!(
        admin_status(&down).await,
        json!({ "engine": "unavailable" })
    );
    let downloads = down.server.admin_client().get("/api/v1/downloads").await;
    assert_eq!(downloads.status, StatusCode::OK, "the page keeps working");
    down.cleanup().await;

    let rotated = EngineFixture::start().await;
    *rotated.engine.status_reply.lock().unwrap() = engine_reply();
    rotated.rotate_key();
    assert_eq!(
        admin_status(&rotated).await,
        json!({ "engine": "unavailable" })
    );
    rotated.cleanup().await;

    let garbled = EngineFixture::start().await;
    *garbled.engine.status_reply.lock().unwrap() = json!({ "unexpected": true });
    assert_eq!(
        admin_status(&garbled).await,
        json!({ "engine": "unavailable" })
    );
    garbled.cleanup().await;
}

#[tokio::test]
async fn without_an_engine_the_proxy_says_not_configured() {
    let fx = EngineFixture::without_engine().await;
    assert_eq!(
        admin_status(&fx).await,
        json!({ "engine": "not_configured" })
    );
    fx.cleanup().await;
}
