//! Tests for the download list endpoint, including LEFT JOIN nullable column handling.

mod common;

use common::TestServer;
use serde_json::Value;
use uuid::Uuid;

/// Regression test: downloads without a download_client or api_key must not
/// cause "unexpected null" decode errors when the LEFT JOIN columns are null.
#[tokio::test]
async fn test_list_downloads_with_null_joined_columns() {
    let server = TestServer::start().await;
    let client = server.admin_client();

    // Insert a download directly WITHOUT download_client_id or api_key_id.
    // This means the LEFT JOINs on download_clients and api_keys will produce
    // NULL for download_client_name and api_key_name.
    let download_id = Uuid::now_v7();
    sqlx::query(
        r#"
        INSERT INTO downloads (id, source, protocol, name, download_url, callback_url, metadata, status)
        VALUES ($1, 'test', 'torrent', 'Test Book', 'http://example.com/dl', 'http://callback', '{}'::jsonb, 'queued')
        "#,
    )
    .bind(download_id)
    .execute(&server.pool)
    .await
    .unwrap();

    // GET /api/v1/downloads — this will fail with 500 if LEFT JOIN nullable
    // columns aren't properly handled with "col?" overrides.
    let resp = client.get("/api/v1/downloads").await;
    resp.assert_ok();

    let body: Value = resp.json();
    let items = body["items"].as_array().expect("items should be an array");
    assert!(!items.is_empty(), "should have at least one download");

    let item = &items[0];
    assert_eq!(item["id"], download_id.to_string());
    assert_eq!(item["name"], "Test Book");
    // These should be null, not cause a decode error
    assert!(item["download_client_name"].is_null());
    assert!(item["api_key_name"].is_null());
}

/// Verify that downloads WITH a download_client still return the joined name.
#[tokio::test]
async fn test_list_downloads_with_joined_client_name() {
    let server = TestServer::start().await;
    let client = server.admin_client();

    // Create a download client
    let client_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO download_clients (id, name, client_type, protocol, base_url) VALUES ($1, $2, 'transmission', 'torrent', 'http://localhost:9091')",
    )
    .bind(client_id)
    .bind("My Transmission")
    .execute(&server.pool)
    .await
    .unwrap();

    // Insert a download WITH the client
    let download_id = Uuid::now_v7();
    sqlx::query(
        r#"
        INSERT INTO downloads (id, source, protocol, name, download_url, callback_url, metadata, status, download_client_id)
        VALUES ($1, 'test', 'torrent', 'Book With Client', 'http://example.com/dl', 'http://callback', '{}'::jsonb, 'queued', $2)
        "#,
    )
    .bind(download_id)
    .bind(client_id)
    .execute(&server.pool)
    .await
    .unwrap();

    let resp = client.get("/api/v1/downloads").await;
    resp.assert_ok();

    let body: Value = resp.json();
    let items = body["items"].as_array().unwrap();
    let item = items
        .iter()
        .find(|i| i["id"] == download_id.to_string())
        .expect("should find the download");

    assert_eq!(item["download_client_name"], "My Transmission");
}
