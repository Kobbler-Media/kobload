//! Newznab proxy endpoint tests.

use crate::common;

use reqwest::StatusCode;
use serde_json::json;

use common::TestServer;

#[tokio::test]
async fn test_newznab_caps_response() {
    let server = TestServer::start().await;

    let create = server
        .admin_client()
        .post_json(
            "/api/v1/indexer-sources",
            &json!({
                "name": "Test Newznab",
                "handler_type": "newznab",
                "handler_config": {
                    "base_url": "http://example.com"
                },
                "category_ids": null,
                "media_type_ids": [],
                "enabled": true
            }),
        )
        .await;
    create.assert_ok();
    let created: serde_json::Value = create.json();
    let id = created["id"].as_str().expect("id");

    let caps = server
        .admin_client()
        .get(&format!("/api/v1/indexer/{id}/newznab?t=caps"))
        .await;
    caps.assert_status(StatusCode::OK);
    let body = caps.text();
    assert!(body.contains("<caps>"), "caps missing: {body}");
    assert!(
        caps.headers
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or_default()
            .contains("application/xml"),
        "content-type missing or incorrect"
    );

    server.cleanup().await;
}
