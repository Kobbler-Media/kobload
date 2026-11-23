//! SAB and the engine can share a provider account. Its cap holds whichever is enabled:
//! a SAB that alone fills the cap is refused, and otherwise the engine shrinks to what SAB
//! leaves while SAB is enabled, which is what makes the SAB rollback safe.

use std::sync::{Arc, Mutex};

use axum::extract::{Query, State};
use reqwest::StatusCode;
use serde_json::{Value, json};
use tokio::net::TcpListener;

use crate::common::usenet::{CONTROL_KEY, UsenetRig};
use usenet_proto::EngineConfigResponse;

const SAB_KEY: &str = "sab-api-key-under-test";
const CLIENTS: &str = "/api/v1/admin/download-clients";
const SERVERS: &str = "/api/v1/admin/usenet/servers";

type SabServers = Arc<Mutex<Value>>;

async fn sab_api(
    State(servers): State<SabServers>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    if q.get("apikey").map(String::as_str) != Some(SAB_KEY) {
        return axum::http::StatusCode::FORBIDDEN.into_response();
    }
    if q.get("mode").map(String::as_str) == Some("get_config") {
        let servers = servers.lock().unwrap().clone();
        return axum::Json(json!({ "config": { "servers": servers } })).into_response();
    }
    axum::Json(json!({ "version": "4.5.0" })).into_response()
}

async fn mock_sab(servers: Value) -> (String, SabServers) {
    let servers = Arc::new(Mutex::new(servers));
    let app = axum::Router::new()
        .route("/api", axum::routing::get(sab_api))
        .with_state(Arc::clone(&servers));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.ok() });
    (url, servers)
}

fn sab_server(host: &str, username: &str, connections: u32) -> Value {
    json!({ "name": host, "host": host, "port": 563, "username": username,
            "password": "**********", "connections": connections, "enable": 1 })
}

fn engine_server(host: &str, backbone: Option<&str>, connections: u32, cap: u32) -> Value {
    json!({
        "name": host, "host": host, "port": 563, "tls": true, "verify_cert": true,
        "connections": connections, "priority": 0, "backbone": backbone,
        "username": "alice", "password": "provider-password",
        "account_connection_cap": cap, "enabled": true
    })
}

async fn add_server(rig: &UsenetRig, body: &Value) -> (StatusCode, String) {
    let resp = rig.tv.server.admin_client().post_json(SERVERS, body).await;
    (resp.status, resp.text())
}

async fn add_sab(rig: &UsenetRig, url: &str, enabled: bool) -> (StatusCode, String) {
    let resp = rig
        .tv
        .server
        .admin_client()
        .post_json(
            CLIENTS,
            &json!({ "name": "SAB", "client_type": "sabnzbd", "base_url": url,
                     "api_key": SAB_KEY, "category": "tv", "enabled": enabled }),
        )
        .await;
    (resp.status, resp.text())
}

async fn patch_sab(rig: &UsenetRig, id: &str, body: &Value) -> (StatusCode, String) {
    let resp = rig
        .tv
        .server
        .admin_client()
        .patch_json(&format!("{CLIENTS}/{id}"), body)
        .await;
    (resp.status, resp.text())
}

async fn pulled(rig: &UsenetRig) -> Vec<(String, u32)> {
    let resp = crate::common::TestClient::new(&rig.tv.server.api_url)
        .with_auth(CONTROL_KEY)
        .get("/api/v1/usenet/engine-config")
        .await;
    assert_eq!(resp.status, StatusCode::OK, "{}", resp.text());
    let config: EngineConfigResponse = serde_json::from_str(&resp.text()).unwrap();
    let mut servers: Vec<(String, u32)> = config
        .servers
        .into_iter()
        .map(|s| (s.host, s.connections))
        .collect();
    servers.sort();
    servers
}

fn id_of(body: &str) -> String {
    serde_json::from_str::<Value>(body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string()
}

#[tokio::test]
async fn test_enabling_sab_shrinks_the_engine_and_disabling_it_restores_the_engine() {
    let rig = UsenetRig::start().await;
    let (status, body) = add_server(&rig, &engine_server("news.a.test", None, 49, 50)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = add_server(&rig, &engine_server("news.other.test", None, 20, 30)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (url, _) = mock_sab(json!([sab_server("news.a.test", "alice", 49)])).await;

    let (status, body) = add_sab(&rig, &url, false).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let sab = id_of(&body);
    let full = vec![
        ("news.a.test".to_string(), 49),
        ("news.other.test".to_string(), 20),
    ];
    assert_eq!(pulled(&rig).await, full);

    let (status, body) =
        patch_sab(&rig, &sab, &json!({ "enabled": true, "is_default": true })).await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "the rollback is allowed: {body}"
    );
    assert_eq!(
        pulled(&rig).await,
        [
            ("news.a.test".to_string(), 1),
            ("news.other.test".to_string(), 20)
        ],
        "the engine keeps what SAB's 49 leave of the cap of 50"
    );

    let (status, body) = patch_sab(&rig, &sab, &json!({ "enabled": false })).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert_eq!(pulled(&rig).await, full);
    rig.tv.server.cleanup().await;
}

#[tokio::test]
async fn test_a_sab_that_fills_the_cap_alone_is_refused_on_create_enable_and_edit() {
    let rig = UsenetRig::start().await;
    let (status, body) = add_server(&rig, &engine_server("news.a.test", None, 10, 50)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (full_url, _) = mock_sab(json!([sab_server("NEWS.A.test", "alice", 50)])).await;
    let (fair_url, _) = mock_sab(json!([sab_server("news.a.test", "alice", 40)])).await;

    let (status, body) = add_sab(&rig, &full_url, true).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(
        body.contains("news.a.test") && body.contains("50"),
        "{body}"
    );
    assert!(
        !body.contains("alice"),
        "a refusal never echoes the login: {body}"
    );

    let (status, body) = add_sab(&rig, &full_url, false).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a disabled SAB holds nothing: {body}"
    );
    let disabled = id_of(&body);
    let (status, body) = patch_sab(&rig, &disabled, &json!({ "enabled": true })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.contains("news.a.test"), "{body}");

    let (status, body) = add_sab(&rig, &fair_url, true).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let fair = id_of(&body);
    let (status, body) = patch_sab(&rig, &fair, &json!({ "base_url": full_url })).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    let (status, body) = patch_sab(&rig, &fair, &json!({ "name": "SAB renamed" })).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    rig.tv.server.cleanup().await;
}

#[tokio::test]
async fn test_an_enabled_sab_that_stops_answering_keeps_its_last_known_share() {
    let rig = UsenetRig::start().await;
    let (status, body) = add_server(&rig, &engine_server("news.a.test", None, 49, 50)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (url, sab_servers) = mock_sab(json!([sab_server("news.a.test", "alice", 40)])).await;
    let (status, body) = add_sab(&rig, &url, true).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let shrunk = vec![("news.a.test".to_string(), 10)];
    assert_eq!(pulled(&rig).await, shrunk);

    *sab_servers.lock().unwrap() = Value::Null;
    assert_eq!(
        pulled(&rig).await,
        shrunk,
        "SAB's connections may still be open while its API is down"
    );
    rig.tv.server.cleanup().await;
}

#[tokio::test]
async fn test_one_login_on_two_hosts_of_a_backbone_is_one_account() {
    let rig = UsenetRig::start().await;
    let (status, body) =
        add_server(&rig, &engine_server("news.a.test", Some("Omicron"), 20, 30)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = add_server(
        &rig,
        &engine_server("eu.news.a.test", Some("omicron"), 15, 30),
    )
    .await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "20 + 15 on one login: {body}"
    );
    assert!(!body.contains("alice"), "{body}");
    let (status, body) = add_server(
        &rig,
        &engine_server("eu.news.a.test", Some("omicron"), 10, 30),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) =
        add_server(&rig, &engine_server("news.b.test", Some("other"), 25, 30)).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "another backbone is another account: {body}"
    );

    let (url, _) = mock_sab(json!([sab_server("eu.news.a.test", "alice", 20)])).await;
    let (status, body) = add_sab(&rig, &url, true).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let pulled = pulled(&rig).await;
    let total: u32 = pulled
        .iter()
        .filter(|(host, _)| host.ends_with("news.a.test"))
        .map(|(_, c)| c)
        .sum();
    assert_eq!(
        total, 10,
        "SAB's 20 on the alias host leave the backbone account 10: {pulled:?}"
    );
    rig.tv.server.cleanup().await;
}
