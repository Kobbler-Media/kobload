//! Usenet servers are configured in kobload and pulled by the engine. Only the engine's own
//! key can read them back with passwords; everything the admin UI sees is masked.

use std::sync::Arc;

use reqwest::StatusCode;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

use crate::common::TestServer;
use crate::common::usenet::{CONTROL_KEY, UsenetRig};
use kobload_api::debrid::DefaultDebridProviderResolver;
use usenet_proto::EngineConfigResponse;

const PASSWORD: &str = "hunter2-provider-password";
const SERVERS: &str = "/api/v1/admin/usenet/servers";
const ENGINE_CONFIG: &str = "/api/v1/usenet/engine-config";

fn server(name: &str, username: &str, connections: u32) -> Value {
    json!({
        "name": name, "host": "news.example.test", "port": 563, "tls": true,
        "verify_cert": true, "ip_preference": "v4", "connections": connections,
        "priority": 0, "backbone": "omicron", "retention_days": 5000,
        "expires_on": "2027-01-31", "quota_bytes": null, "username": username,
        "password": PASSWORD, "account_connection_cap": 30, "enabled": true
    })
}

async fn create(rig: &UsenetRig, body: &Value) -> (StatusCode, String) {
    let resp = rig.tv.server.admin_client().post_json(SERVERS, body).await;
    (resp.status, resp.text())
}

async fn engine_config(rig: &UsenetRig, token: Option<&str>) -> (StatusCode, String) {
    let client = crate::common::TestClient::new(&rig.tv.server.api_url);
    let resp = match token {
        Some(t) => client.with_auth(t).get(ENGINE_CONFIG).await,
        None => client.get(ENGINE_CONFIG).await,
    };
    (resp.status, resp.text())
}

#[tokio::test]
async fn test_engine_config_needs_the_control_key_and_the_admin_list_masks_passwords() {
    let rig = UsenetRig::start().await;
    let (status, body) = create(&rig, &server("Primary", "alice", 20)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let mut backup = server("Block", "bob", 8);
    backup["priority"] = json!(99);
    backup["quota_bytes"] = json!(500_000_000_000i64);
    backup["enabled"] = json!(false);
    let (status, body) = create(&rig, &backup).await;
    assert_eq!(status, StatusCode::OK, "{body}");

    let resp = rig.tv.server.admin_client().get(SERVERS).await;
    resp.assert_ok();
    let listed = resp.text();
    assert!(!listed.contains(PASSWORD), "{listed}");
    let rows: Vec<Value> = serde_json::from_str(&listed).unwrap();
    assert_eq!(rows.len(), 2);
    for row in &rows {
        assert!(row.get("password").is_none(), "{row}");
        assert_eq!(row["has_password"], true);
    }
    let primary = rows.iter().find(|r| r["name"] == "Primary").unwrap();
    assert_eq!(primary["ip_preference"], "v4");
    assert_eq!(primary["expires_on"], "2027-01-31");
    assert_eq!(primary["account_connection_cap"], 30);
    let id = primary["id"].as_str().unwrap().to_string();
    let resp = rig
        .tv
        .server
        .admin_client()
        .get(&format!("{SERVERS}/{id}"))
        .await;
    resp.assert_ok();
    assert!(!resp.text().contains(PASSWORD), "{}", resp.text());

    for token in [
        None,
        Some(rig.tv.server.admin_token.as_str()),
        Some("wrong"),
    ] {
        let (status, body) = engine_config(&rig, token).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "{token:?}: {body}");
        assert!(!body.contains(PASSWORD));
    }
    let (status, body) = engine_config(&rig, Some(CONTROL_KEY)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let config: EngineConfigResponse = serde_json::from_str(&body).unwrap();
    assert_eq!(config.servers.len(), 1, "disabled servers stay out");
    let pulled = &config.servers[0];
    assert_eq!(pulled.id, id);
    assert_eq!(pulled.password, PASSWORD);
    assert_eq!(pulled.username, "alice");
    assert!(pulled.tls && pulled.verify_cert);
    assert_eq!(pulled.ip_preference, usenet_proto::IpPreference::V4);
    assert_eq!(pulled.backbone.as_deref(), Some("omicron"));
    assert_eq!(pulled.retention_days, Some(5000));
    assert_eq!(pulled.expires_on.as_deref(), Some("2027-01-31"));

    let mut renamed = server("Primary EU", "alice", 20);
    renamed["password"] = Value::Null;
    let resp = rig
        .tv
        .server
        .admin_client()
        .put_json(&format!("{SERVERS}/{id}"), &renamed)
        .await;
    assert_eq!(resp.status, StatusCode::NO_CONTENT, "{}", resp.text());
    let (_, body) = engine_config(&rig, Some(CONTROL_KEY)).await;
    let config: EngineConfigResponse = serde_json::from_str(&body).unwrap();
    assert_eq!(config.servers[0].name, "Primary EU");
    assert_eq!(
        config.servers[0].password, PASSWORD,
        "a blank password keeps the old one"
    );

    let resp = rig
        .tv
        .server
        .admin_client()
        .delete(&format!("{SERVERS}/{id}"))
        .await;
    assert_eq!(resp.status, StatusCode::NO_CONTENT);
    let (_, body) = engine_config(&rig, Some(CONTROL_KEY)).await;
    let config: EngineConfigResponse = serde_json::from_str(&body).unwrap();
    assert!(config.servers.is_empty());
    rig.tv.server.cleanup().await;
}

#[tokio::test]
async fn test_an_account_cannot_be_given_more_connections_than_its_cap() {
    let rig = UsenetRig::start().await;
    let (status, body) = create(&rig, &server("Primary", "alice", 20)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let (status, body) = create(&rig, &server("Primary 2", "alice", 15)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert!(body.contains("30"), "{body}");
    assert!(body.contains("news.example.test"), "{body}");
    assert!(
        !body.contains("alice"),
        "a refusal never echoes the login: {body}"
    );
    let (status, body) = create(&rig, &server("Other login", "carol", 15)).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "another login has its own cap: {body}"
    );
    let (status, body) = create(&rig, &server("Primary 2", "alice", 10)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let id = serde_json::from_str::<Value>(&body).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();

    let resp = rig
        .tv
        .server
        .admin_client()
        .put_json(
            &format!("{SERVERS}/{id}"),
            &server("Primary 2", "alice", 11),
        )
        .await;
    assert_eq!(resp.status, StatusCode::BAD_REQUEST, "{}", resp.text());
    let mut disabled = server("Primary 2", "alice", 11);
    disabled["enabled"] = json!(false);
    let resp = rig
        .tv
        .server
        .admin_client()
        .put_json(&format!("{SERVERS}/{id}"), &disabled)
        .await;
    assert_eq!(
        resp.status,
        StatusCode::NO_CONTENT,
        "a disabled server holds no connections: {}",
        resp.text()
    );
    rig.tv.server.cleanup().await;
}

async fn fake_nntp(password: &'static str) -> u16 {
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
                    } else if line == format!("AUTHINFO PASS {password}") {
                        b"281 welcome\r\n"
                    } else if line.starts_with("AUTHINFO PASS") {
                        b"481 authentication failed\r\n"
                    } else {
                        b"205 bye\r\n"
                    };
                    write.write_all(reply).await.ok();
                }
            });
        }
    });
    port
}

#[tokio::test]
async fn test_a_server_test_logs_in_over_nntp() {
    let rig = UsenetRig::start().await;
    let port = fake_nntp(PASSWORD).await;
    let mut body = server("Local", "alice", 4);
    body["host"] = json!("127.0.0.1");
    body["port"] = json!(port);
    body["tls"] = json!(false);
    let resp = rig
        .tv
        .server
        .admin_client()
        .post_json(&format!("{SERVERS}/test"), &body)
        .await;
    resp.assert_ok();
    let result: Value = resp.json();
    assert_eq!(result["success"], true, "{result}");

    let (status, created) = create(&rig, &body).await;
    assert_eq!(status, StatusCode::OK, "{created}");
    let id = serde_json::from_str::<Value>(&created).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let resp = rig
        .tv
        .server
        .admin_client()
        .post(&format!("{SERVERS}/{id}/test"))
        .await;
    resp.assert_ok();
    let result: Value = resp.json();
    assert_eq!(result["success"], true, "{result}");

    body["password"] = json!("wrong");
    let resp = rig
        .tv
        .server
        .admin_client()
        .post_json(&format!("{SERVERS}/test"), &body)
        .await;
    let result: Value = resp.json();
    assert_eq!(result["success"], false, "{result}");
    assert!(!result.to_string().contains("wrong"), "{result}");
    rig.tv.server.cleanup().await;
}

#[tokio::test]
async fn test_the_usenet_provider_needs_no_api_key_and_tests_the_engine() {
    let rig = UsenetRig::start().await;
    let resp = rig
        .tv
        .server
        .admin_client()
        .post_json(
            "/api/v1/admin/debrid/providers",
            &json!({ "name": "Own usenet", "provider_type": "usenet", "api_key": "" }),
        )
        .await;
    resp.assert_ok();
    let id = resp.json::<Value>()["id"].as_str().unwrap().to_string();
    let resp = rig
        .tv
        .server
        .admin_client()
        .post(&format!("/api/v1/admin/debrid/providers/{id}/test"))
        .await;
    resp.assert_ok();
    let result: Value = resp.json();
    assert_eq!(result["success"], true, "{result}");
    rig.tv.server.cleanup().await;
}

#[tokio::test]
async fn test_without_an_engine_the_config_endpoint_and_provider_test_refuse() {
    let server =
        TestServer::start_with_debrid(Arc::new(DefaultDebridProviderResolver::default())).await;
    let resp = crate::common::TestClient::new(&server.api_url)
        .with_auth(CONTROL_KEY)
        .get(ENGINE_CONFIG)
        .await;
    assert_eq!(resp.status, StatusCode::NOT_FOUND, "{}", resp.text());
    let resp = server
        .admin_client()
        .post_json(
            "/api/v1/admin/debrid/providers/test",
            &json!({ "provider_type": "usenet", "api_key": "" }),
        )
        .await;
    let result: Value = resp.json();
    assert_eq!(result["success"], false, "{result}");
    server.cleanup().await;
}

async fn created_id(rig: &UsenetRig, body: &Value) -> String {
    let (status, created) = create(rig, body).await;
    assert_eq!(status, StatusCode::OK, "{created}");
    serde_json::from_str::<Value>(&created).unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string()
}

async fn update(rig: &UsenetRig, id: &str, body: &Value) -> (StatusCode, String) {
    let resp = rig
        .tv
        .server
        .admin_client()
        .put_json(&format!("{SERVERS}/{id}"), body)
        .await;
    (resp.status, resp.text())
}

async fn test_body(rig: &UsenetRig, body: &Value) -> (StatusCode, Value) {
    let resp = rig
        .tv
        .server
        .admin_client()
        .post_json(&format!("{SERVERS}/test"), body)
        .await;
    let status = resp.status;
    let text = resp.text();
    (
        status,
        serde_json::from_str(&text).unwrap_or(Value::String(text)),
    )
}

#[tokio::test]
async fn test_moving_a_server_to_another_endpoint_needs_its_password_again() {
    let rig = UsenetRig::start().await;
    let id = created_id(&rig, &server("Primary", "alice", 10)).await;
    let without_password = |field: &str, value: Value| {
        let mut body = server("Primary", "alice", 10);
        body["password"] = Value::Null;
        body[field] = value;
        body
    };

    for (field, value) in [
        ("host", json!("news.elsewhere.test")),
        ("port", json!(443)),
        ("tls", json!(false)),
        ("verify_cert", json!(false)),
    ] {
        let (status, body) = update(&rig, &id, &without_password(field, value.clone())).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "{field}: {body}");
        assert!(body.to_lowercase().contains("password"), "{field}: {body}");
    }
    let (status, body) = update(
        &rig,
        &id,
        &without_password("host", json!(" NEWS.Example.test ")),
    )
    .await;
    assert_eq!(status, StatusCode::NO_CONTENT, "the same host: {body}");
    let (status, body) = update(&rig, &id, &without_password("connections", json!(12))).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");

    let mut moved = server("Primary", "alice", 10);
    moved["host"] = json!("news.elsewhere.test");
    moved["password"] = json!("a-new-provider-password");
    let (status, body) = update(&rig, &id, &moved).await;
    assert_eq!(status, StatusCode::NO_CONTENT, "{body}");
    let (_, body) = engine_config(&rig, Some(CONTROL_KEY)).await;
    let config: EngineConfigResponse = serde_json::from_str(&body).unwrap();
    assert_eq!(config.servers[0].host, "news.elsewhere.test");
    assert_eq!(config.servers[0].password, "a-new-provider-password");
    rig.tv.server.cleanup().await;
}

#[tokio::test]
async fn test_line_breaks_in_credentials_are_refused() {
    let rig = UsenetRig::start().await;
    let id = created_id(&rig, &server("Primary", "alice", 10)).await;
    for (field, value) in [
        ("username", "alice\r\nQUIT"),
        ("username", "alice\n"),
        ("password", "secret\rAUTHINFO"),
        ("password", "secret\nQUIT"),
    ] {
        let mut body = server("Primary", "alice", 10);
        body[field] = json!(value);
        let (status, text) = create(&rig, &body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "create {field}: {text}");
        let (status, text) = update(&rig, &id, &body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "update {field}: {text}");
        let (status, result) = test_body(&rig, &body).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "test {field}: {result}");
    }
    rig.tv.server.cleanup().await;
}

#[tokio::test]
async fn test_an_edited_server_is_tested_where_the_form_points() {
    let rig = UsenetRig::start().await;
    let stored = fake_nntp(PASSWORD).await;
    let elsewhere = fake_nntp("elsewhere-password").await;
    let local = |port: u16, password: Value| {
        let mut body = server("Local", "alice", 4);
        body["host"] = json!("127.0.0.1");
        body["port"] = json!(port);
        body["tls"] = json!(false);
        body["password"] = password;
        body
    };
    let id = created_id(&rig, &local(stored, json!(PASSWORD))).await;

    let mut edited = local(stored, Value::Null);
    edited["id"] = json!(id);
    edited["username"] = json!("alice-renamed");
    let (status, result) = test_body(&rig, &edited).await;
    assert_eq!(status, StatusCode::OK, "{result}");
    assert_eq!(
        result["success"], true,
        "the stored password is used: {result}"
    );

    let mut moved = local(elsewhere, Value::Null);
    moved["id"] = json!(id);
    let (_, result) = test_body(&rig, &moved).await;
    assert_eq!(result["success"], false, "{result}");
    assert!(
        result["message"]
            .as_str()
            .unwrap()
            .to_lowercase()
            .contains("password"),
        "the stored password does not follow a new endpoint: {result}"
    );

    let mut unverified = local(stored, Value::Null);
    unverified["id"] = json!(id);
    unverified["verify_cert"] = json!(false);
    let (_, result) = test_body(&rig, &unverified).await;
    assert_eq!(
        result["success"], false,
        "the stored password is not sent without certificate checks: {result}"
    );

    moved["password"] = json!("elsewhere-password");
    let (_, result) = test_body(&rig, &moved).await;
    assert_eq!(
        result["success"], true,
        "the edited port is tested: {result}"
    );
    rig.tv.server.cleanup().await;
}

const SAB_KEY: &str = "sab-api-key-under-test";

async fn sab_config(
    axum::extract::Query(q): axum::extract::Query<std::collections::HashMap<String, String>>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    if q.get("apikey").map(String::as_str) != Some(SAB_KEY)
        || q.get("mode").map(String::as_str) != Some("get_config")
        || q.get("section").map(String::as_str) != Some("servers")
    {
        return axum::http::StatusCode::FORBIDDEN.into_response();
    }
    axum::Json(json!({ "config": { "servers": [
        { "name": "primary", "displayname": "Primary", "host": " NEWS.Example.TEST", "port": 563,
          "username": "alice", "password": "**********", "connections": 12, "enable": 1 },
        { "name": "spare", "host": "news.example.test", "port": 119,
          "username": "alice", "password": "**********", "connections": "40", "enable": 0 },
        { "name": "other", "host": "news.other.test", "port": 563,
          "username": "alice", "password": "**********", "connections": 50, "enable": 1 }
    ] } }))
    .into_response()
}

async fn mock_sab() -> String {
    let app = axum::Router::new().route("/api", axum::routing::get(sab_config));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.ok() });
    url
}

async fn add_sab(rig: &UsenetRig, base_url: &str) -> uuid::Uuid {
    sqlx::query_scalar(
        "INSERT INTO download_clients (id, name, client_type, protocol, base_url, api_key, enabled, is_default)
         VALUES ($1, 'SAB', 'sabnzbd', 'usenet', $2, $3, true, false) RETURNING id",
    )
    .bind(uuid::Uuid::now_v7())
    .bind(base_url)
    .bind(SAB_KEY)
    .fetch_one(&rig.tv.server.pool)
    .await
    .unwrap()
}

#[tokio::test]
async fn test_sabnzbd_connections_to_the_same_account_count_toward_its_cap() {
    let rig = UsenetRig::start().await;
    let sab = add_sab(&rig, &mock_sab().await).await;

    let id = created_id(&rig, &server("Primary", "alice", 20)).await;
    let (_, body) = engine_config(&rig, Some(CONTROL_KEY)).await;
    let config: EngineConfigResponse = serde_json::from_str(&body).unwrap();
    assert_eq!(
        config.servers[0].connections, 18,
        "the engine yields to SAB's 12"
    );
    let (status, body) = update(&rig, &id, &server("Primary", "alice", 31)).await;
    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "over the cap alone: {body}"
    );
    let (status, body) = create(&rig, &server("Another login", "bob", 30)).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "SAB's login is alice, not bob: {body}"
    );

    sqlx::query("UPDATE download_clients SET enabled = false WHERE id = $1")
        .bind(sab)
        .execute(&rig.tv.server.pool)
        .await
        .unwrap();
    let (status, body) = update(&rig, &id, &server("Primary", "alice", 30)).await;
    assert_eq!(
        status,
        StatusCode::NO_CONTENT,
        "a disabled SAB holds nothing: {body}"
    );
    rig.tv.server.cleanup().await;
}

#[tokio::test]
async fn test_an_unreachable_sabnzbd_does_not_block_a_save() {
    let rig = UsenetRig::start().await;
    let closed = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", closed.local_addr().unwrap());
    drop(closed);
    add_sab(&rig, &url).await;
    let (status, body) = create(&rig, &server("Primary", "alice", 30)).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    rig.tv.server.cleanup().await;
}

// Accepts connections and never answers, so a client that talks to it can only time out.
async fn silent_sab() -> (String, Arc<std::sync::atomic::AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    let contacts = Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let seen = contacts.clone();
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((stream, _)) = listener.accept().await {
            seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            held.push(stream);
        }
    });
    (url, contacts)
}

// A client that past downloads still point at.
async fn reference_client(rig: &UsenetRig, client: uuid::Uuid) {
    let pool = &rig.tv.server.pool;
    sqlx::query(
        "INSERT INTO downloads (id, source, protocol, name, download_url, callback_url, metadata, status, download_client_id)
         VALUES ($1, 'test', 'usenet', 'Past grab', 'http://example.com/dl', 'http://callback', '{}'::jsonb, 'completed', $2)",
    )
    .bind(uuid::Uuid::now_v7())
    .bind(client)
    .execute(pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn test_a_disabled_sabnzbd_is_never_contacted_and_can_be_deleted() {
    let rig = UsenetRig::start().await;
    let (url, contacts) = silent_sab().await;
    let sab = add_sab(&rig, &url).await;
    sqlx::query("UPDATE download_clients SET enabled = false WHERE id = $1")
        .bind(sab)
        .execute(&rig.tv.server.pool)
        .await
        .unwrap();
    reference_client(&rig, sab).await;
    created_id(&rig, &server("Primary", "alice", 20)).await;

    let started = std::time::Instant::now();
    let resp = rig.tv.server.admin_client().get(SERVERS).await;
    resp.assert_ok();
    assert!(
        started.elapsed() < std::time::Duration::from_secs(2),
        "the list waited on a disabled SAB"
    );

    let resp = rig
        .tv
        .server
        .admin_client()
        .delete(&format!("/api/v1/admin/download-clients/{sab}"))
        .await;
    assert_eq!(resp.status, StatusCode::NO_CONTENT, "{}", resp.text());
    assert_eq!(
        contacts.load(std::sync::atomic::Ordering::SeqCst),
        0,
        "a disabled SAB is nothing to kobload"
    );
    rig.tv.server.cleanup().await;
}

#[tokio::test]
async fn test_deleting_a_client_keeps_the_downloads_that_used_it() {
    let rig = UsenetRig::start().await;
    let pool = &rig.tv.server.pool;
    let client: uuid::Uuid = sqlx::query_scalar(
        "INSERT INTO download_clients (id, name, client_type, protocol, base_url, enabled, is_default)
         VALUES ($1, 'Old client', 'transmission', 'torrent', 'http://127.0.0.1:1', true, false) RETURNING id",
    )
    .bind(uuid::Uuid::now_v7())
    .fetch_one(pool)
    .await
    .unwrap();
    reference_client(&rig, client).await;

    let resp = rig
        .tv
        .server
        .admin_client()
        .delete(&format!("/api/v1/admin/download-clients/{client}"))
        .await;
    assert_eq!(resp.status, StatusCode::NO_CONTENT, "{}", resp.text());
    let orphaned: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM downloads WHERE name = 'Past grab' AND download_client_id IS NULL",
    )
    .fetch_one(pool)
    .await
    .unwrap();
    assert_eq!(orphaned, 1, "history outlives the client");
    rig.tv.server.cleanup().await;
}
