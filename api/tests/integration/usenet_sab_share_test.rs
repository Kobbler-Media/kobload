//! SAB and the engine can share a provider account. Its cap holds whichever is enabled:
//! a SAB that alone fills the cap is refused, and otherwise the engine shrinks to what SAB
//! leaves while SAB is enabled, which is what makes the SAB rollback safe.

use std::sync::{Arc, Mutex};

use axum::extract::{Query, State};
use reqwest::StatusCode;
use serde_json::{Value, json};
use sha2::Digest;
use tokio::net::TcpListener;

use crate::common::usenet::{CONTROL_KEY, UsenetRig};
use kobload_api::repositories::app_settings;
use usenet_proto::EngineConfigResponse;

const SAB_KEY: &str = "sab-api-key-under-test";
pub(crate) const CLIENTS: &str = "/api/v1/admin/download-clients";
const SERVERS: &str = "/api/v1/admin/usenet/servers";

#[derive(Default)]
pub(crate) struct Sab {
    pub(crate) servers: Mutex<Value>,
    pub(crate) queue: Mutex<Value>,
    pub(crate) paused: Mutex<bool>,
}

impl Sab {
    pub(crate) fn busy(&self, busy: bool) {
        *self.queue.lock().unwrap() = if busy {
            json!([{ "nzo_id": "SABnzbd_nzo_1", "status": "Downloading" }])
        } else {
            json!([])
        };
    }

    pub(crate) fn go_down(&self) {
        *self.servers.lock().unwrap() = Value::Null;
        *self.queue.lock().unwrap() = Value::Null;
    }
}

async fn sab_api(
    State(sab): State<Arc<Sab>>,
    Query(q): Query<std::collections::HashMap<String, String>>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    if q.get("apikey").map(String::as_str) != Some(SAB_KEY) {
        return axum::http::StatusCode::FORBIDDEN.into_response();
    }
    match q.get("mode").map(String::as_str) {
        Some("get_config") => {
            let servers = sab.servers.lock().unwrap().clone();
            axum::Json(json!({ "config": { "servers": servers } })).into_response()
        }
        Some("queue") => {
            let slots = sab.queue.lock().unwrap().clone();
            if slots.is_null() {
                return axum::http::StatusCode::SERVICE_UNAVAILABLE.into_response();
            }
            let paused = *sab.paused.lock().unwrap();
            axum::Json(json!({ "queue": { "paused": paused, "slots": slots } })).into_response()
        }
        _ => axum::Json(json!({ "version": "4.5.0" })).into_response(),
    }
}

pub(crate) async fn mock_sab(servers: Value) -> (String, Arc<Sab>) {
    let sab = Arc::new(Sab {
        servers: Mutex::new(servers),
        queue: Mutex::new(json!([])),
        paused: Mutex::new(false),
    });
    let app = axum::Router::new()
        .route("/api", axum::routing::get(sab_api))
        .with_state(Arc::clone(&sab));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.ok() });
    (url, sab)
}

pub(crate) fn sab_server(host: &str, username: &str, connections: u32) -> Value {
    json!({ "name": host, "host": host, "port": 563, "username": username,
            "password": "**********", "connections": connections, "enable": 1 })
}

pub(crate) fn engine_server(
    host: &str,
    backbone: Option<&str>,
    connections: u32,
    cap: u32,
) -> Value {
    json!({
        "name": host, "host": host, "port": 563, "tls": true, "verify_cert": true,
        "connections": connections, "priority": 0, "backbone": backbone,
        "username": "alice", "password": "provider-password",
        "account_connection_cap": cap, "enabled": true
    })
}

pub(crate) async fn add_server(rig: &UsenetRig, body: &Value) -> (StatusCode, String) {
    let resp = rig.tv.server.admin_client().post_json(SERVERS, body).await;
    (resp.status, resp.text())
}

pub(crate) async fn add_sab(rig: &UsenetRig, url: &str, enabled: bool) -> (StatusCode, String) {
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

pub(crate) async fn patch_sab(rig: &UsenetRig, id: &str, body: &Value) -> (StatusCode, String) {
    let resp = rig
        .tv
        .server
        .admin_client()
        .patch_json(&format!("{CLIENTS}/{id}"), body)
        .await;
    (resp.status, resp.text())
}

pub(crate) async fn pulled(rig: &UsenetRig) -> Vec<(String, u32)> {
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

pub(crate) fn id_of(body: &str) -> String {
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

    sab_servers.go_down();
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

pub(crate) async fn listed_effective(rig: &UsenetRig, host: &str) -> Value {
    let rows: Vec<Value> = rig.tv.server.admin_client().get(SERVERS).await.json();
    rows.into_iter().find(|r| r["host"] == host).unwrap()["effective_connections"].clone()
}

#[tokio::test]
async fn test_the_admin_list_shows_what_the_engine_is_left_with() {
    let rig = UsenetRig::start().await;
    add_server(&rig, &engine_server("news.a.test", None, 49, 50)).await;
    assert_eq!(listed_effective(&rig, "news.a.test").await, 49);
    let (url, _) = mock_sab(json!([sab_server("news.a.test", "alice", 49)])).await;
    let (status, body) = add_sab(&rig, &url, true).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(listed_effective(&rig, "news.a.test").await, 1);
    rig.tv.server.cleanup().await;
}

#[tokio::test]
async fn test_a_disabled_sab_keeps_its_share_until_its_queue_drains() {
    let rig = UsenetRig::start().await;
    add_server(&rig, &engine_server("news.a.test", None, 49, 50)).await;
    let (url, sab) = mock_sab(json!([sab_server("news.a.test", "alice", 40)])).await;
    let (_, body) = add_sab(&rig, &url, true).await;
    let id = id_of(&body);
    sab.busy(true);

    let (status, body) = patch_sab(&rig, &id, &json!({ "enabled": false })).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    assert_eq!(
        pulled(&rig).await,
        [("news.a.test".to_string(), 10)],
        "SAB is still downloading its queue"
    );
    sab.busy(false);
    assert_eq!(pulled(&rig).await, [("news.a.test".to_string(), 49)]);
    rig.tv.server.cleanup().await;
}

#[tokio::test]
async fn test_the_last_known_share_survives_a_restart_while_sab_is_down() {
    let rig = UsenetRig::start().await;
    add_server(&rig, &engine_server("news.a.test", None, 49, 50)).await;
    let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", closed.local_addr().unwrap());
    drop(closed);
    let id = uuid::Uuid::now_v7();
    sqlx::query(
        "INSERT INTO download_clients (id, name, client_type, protocol, base_url, api_key, enabled, is_default)
         VALUES ($1, 'SAB', 'sabnzbd', 'usenet', $2, $3, true, false)",
    )
    .bind(id)
    .bind(&url)
    .bind(SAB_KEY)
    .execute(&rig.tv.server.pool)
    .await
    .unwrap();
    let login = const_hex::encode(sha2::Sha256::digest("alice"));
    let stored = json!({
        "base_url": url, "draining": false, "seen_at": 1,
        "servers": [{ "host": "news.a.test", "username": login, "connections": 40 }],
    });
    app_settings::set(
        &rig.tv.server.pool,
        &format!("usenet_sab_share:{id}"),
        &stored,
    )
    .await
    .unwrap();
    assert_eq!(pulled(&rig).await, [("news.a.test".to_string(), 10)]);
    rig.tv.server.cleanup().await;
}

#[tokio::test]
async fn test_the_stored_share_never_holds_a_login() {
    let rig = UsenetRig::start().await;
    add_server(&rig, &engine_server("news.a.test", None, 49, 50)).await;
    let (url, _) = mock_sab(json!([sab_server("news.a.test", "alice", 40)])).await;
    let (status, body) = add_sab(&rig, &url, true).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    pulled(&rig).await;
    let stored = app_settings::get(
        &rig.tv.server.pool,
        &format!("usenet_sab_share:{}", id_of(&body)),
    )
    .await
    .unwrap()
    .expect("the share is stored");
    assert!(!stored.to_string().contains("alice"), "{stored}");
    rig.tv.server.cleanup().await;
}

#[tokio::test]
async fn test_a_sab_change_asks_the_engine_to_pull_at_once() {
    let rig = UsenetRig::start().await;
    add_server(&rig, &engine_server("news.a.test", None, 10, 50)).await;
    let (url, _) = mock_sab(json!([sab_server("news.a.test", "alice", 10)])).await;
    let (_, body) = add_sab(&rig, &url, false).await;
    let before = rig.engine.config_refreshes();
    assert!(before >= 1, "creating a client asks for a pull");
    patch_sab(&rig, &id_of(&body), &json!({ "enabled": true })).await;
    assert_eq!(rig.engine.config_refreshes(), before + 1);
    rig.tv.server.cleanup().await;
}

#[tokio::test]
async fn test_a_shared_backbone_without_a_login_is_not_one_account() {
    let rig = UsenetRig::start().await;
    let anonymous = |host: &str, connections: u32| {
        let mut body = engine_server(host, Some("omicron"), connections, 30);
        body["username"] = json!("");
        body
    };
    let (status, body) = add_server(&rig, &anonymous("news.a.test", 20)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = add_server(&rig, &anonymous("news.b.test", 20)).await;
    assert_eq!(status, StatusCode::OK, "no login to share: {body}");
    rig.tv.server.cleanup().await;
}

#[tokio::test]
async fn test_engine_edits_during_a_rollback_are_judged_on_what_the_engine_would_hold() {
    let rig = UsenetRig::start().await;
    let (status, body) = add_server(&rig, &engine_server("news.a.test", None, 10, 50)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let rows: Vec<Value> = rig.tv.server.admin_client().get(SERVERS).await.json();
    let server = rows[0]["id"].as_str().unwrap().to_string();
    let (url, _) = mock_sab(json!([sab_server("news.a.test", "alice", 40)])).await;
    add_sab(&rig, &url, true).await;

    let edit = |connections: u32| {
        let mut body = engine_server("news.a.test", None, connections, 50);
        body["password"] = Value::Null;
        body
    };
    let path = format!("{SERVERS}/{server}");
    let resp = rig
        .tv
        .server
        .admin_client()
        .put_json(&path, &edit(49))
        .await;
    assert_eq!(resp.status, StatusCode::NO_CONTENT, "{}", resp.text());
    assert_eq!(pulled(&rig).await, [("news.a.test".to_string(), 10)]);
    let resp = rig
        .tv
        .server
        .admin_client()
        .put_json(&path, &edit(51))
        .await;
    assert_eq!(resp.status, StatusCode::BAD_REQUEST, "over the cap alone");
    rig.tv.server.cleanup().await;
}

#[tokio::test]
async fn test_a_sab_is_only_refused_for_a_change_that_matters_on_a_live_account() {
    let rig = UsenetRig::start().await;
    let mut idle = engine_server("news.idle.test", None, 10, 50);
    idle["enabled"] = json!(false);
    add_server(&rig, &idle).await;
    let (url, _) = mock_sab(json!([sab_server("news.idle.test", "alice", 60)])).await;
    let (status, body) = add_sab(&rig, &url, true).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "no engine server is enabled on this account: {body}"
    );
    let sab = id_of(&body);

    add_server(&rig, &engine_server("news.a.test", None, 10, 50)).await;
    sqlx::query("UPDATE usenet_servers SET enabled = true WHERE host = 'news.idle.test'")
        .execute(&rig.tv.server.pool)
        .await
        .unwrap();
    let (status, body) = patch_sab(&rig, &sab, &json!({ "name": "Renamed" })).await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "a rename changes nothing: {body}"
    );
    let (status, body) = patch_sab(&rig, &sab, &json!({ "enabled": true })).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "already enabled: {body}");
    rig.tv.server.cleanup().await;
}
