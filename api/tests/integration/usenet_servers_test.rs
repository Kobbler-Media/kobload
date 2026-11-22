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
