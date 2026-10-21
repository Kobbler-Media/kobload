//! Watch directory CRUD API tests.

use crate::common;

use reqwest::StatusCode;
use serde_json::{Value, json};

use common::TestServer;

fn watch_dir_payload(path: &str) -> Value {
    json!({
        "path": path,
        "name": "Test Watch Dir",
        "media_type": "book",
        "callback_url": "http://localhost:6766/api/v1/admin/ingest/test-token",
        "recursive": true,
        "enabled": true
    })
}

#[tokio::test]
async fn test_create_watch_directory() {
    let server = TestServer::start().await;

    let resp = server
        .admin_client()
        .post_json(
            "/api/v1/watch-directories",
            &watch_dir_payload("/tmp/test-books"),
        )
        .await;

    resp.assert_status(StatusCode::CREATED);
    let json: Value = resp.json();
    assert_eq!(json["name"], "Test Watch Dir");
    assert_eq!(json["path"], "/tmp/test-books");
    assert_eq!(json["media_type"], "book");
    assert_eq!(json["scan_status"], "idle");
    assert!(json["id"].is_string());

    server.cleanup().await;
}

#[tokio::test]
async fn test_create_watch_directory_requires_auth() {
    let server = TestServer::start().await;

    let resp = server
        .anon_client()
        .post_json(
            "/api/v1/watch-directories",
            &watch_dir_payload("/tmp/unauthed"),
        )
        .await;

    resp.assert_status(StatusCode::UNAUTHORIZED);

    server.cleanup().await;
}

#[tokio::test]
async fn test_create_watch_directory_missing_path() {
    let server = TestServer::start().await;

    let resp = server
        .admin_client()
        .post_json(
            "/api/v1/watch-directories",
            &json!({
                "name": "No Path",
                "media_type": "book",
                "callback_url": "http://localhost/ingest"
            }),
        )
        .await;

    assert!(resp.status.is_client_error());

    server.cleanup().await;
}

#[tokio::test]
async fn test_create_duplicate_path_fails() {
    let server = TestServer::start().await;

    let resp = server
        .admin_client()
        .post_json(
            "/api/v1/watch-directories",
            &watch_dir_payload("/tmp/dup-test"),
        )
        .await;
    resp.assert_status(StatusCode::CREATED);

    let resp = server
        .admin_client()
        .post_json(
            "/api/v1/watch-directories",
            &watch_dir_payload("/tmp/dup-test"),
        )
        .await;

    // Should fail due to unique constraint on path
    assert!(resp.status.is_client_error() || resp.status.is_server_error());

    server.cleanup().await;
}

#[tokio::test]
async fn test_list_watch_directories() {
    let server = TestServer::start().await;

    // Create two directories
    server
        .admin_client()
        .post_json(
            "/api/v1/watch-directories",
            &watch_dir_payload("/tmp/list-test-1"),
        )
        .await
        .assert_status(StatusCode::CREATED);
    server
        .admin_client()
        .post_json(
            "/api/v1/watch-directories",
            &watch_dir_payload("/tmp/list-test-2"),
        )
        .await
        .assert_status(StatusCode::CREATED);

    let resp = server.admin_client().get("/api/v1/watch-directories").await;
    resp.assert_ok();
    let dirs: Vec<Value> = resp.json();
    assert!(dirs.len() >= 2);

    server.cleanup().await;
}

#[tokio::test]
async fn test_list_watch_directories_requires_auth() {
    let server = TestServer::start().await;

    let resp = server.anon_client().get("/api/v1/watch-directories").await;
    resp.assert_status(StatusCode::UNAUTHORIZED);

    server.cleanup().await;
}

#[tokio::test]
async fn test_get_watch_directory() {
    let server = TestServer::start().await;

    let resp = server
        .admin_client()
        .post_json(
            "/api/v1/watch-directories",
            &watch_dir_payload("/tmp/get-test"),
        )
        .await;
    resp.assert_status(StatusCode::CREATED);
    let created: Value = resp.json();
    let id = created["id"].as_str().unwrap();

    let resp = server
        .admin_client()
        .get(&format!("/api/v1/watch-directories/{}", id))
        .await;
    resp.assert_ok();
    let dir: Value = resp.json();
    assert_eq!(dir["id"], id);
    assert_eq!(dir["name"], "Test Watch Dir");
    // Should include file_counts
    assert!(dir["file_counts"].is_array());

    server.cleanup().await;
}

#[tokio::test]
async fn test_get_watch_directory_not_found() {
    let server = TestServer::start().await;

    let fake_id = uuid::Uuid::new_v4();
    let resp = server
        .admin_client()
        .get(&format!("/api/v1/watch-directories/{}", fake_id))
        .await;
    resp.assert_status(StatusCode::NOT_FOUND);

    server.cleanup().await;
}

#[tokio::test]
async fn test_update_watch_directory() {
    let server = TestServer::start().await;

    let resp = server
        .admin_client()
        .post_json(
            "/api/v1/watch-directories",
            &watch_dir_payload("/tmp/update-test"),
        )
        .await;
    resp.assert_status(StatusCode::CREATED);
    let created: Value = resp.json();
    let id = created["id"].as_str().unwrap();

    let resp = server
        .admin_client()
        .patch_json(
            &format!("/api/v1/watch-directories/{}", id),
            &json!({
                "name": "Updated Name",
                "enabled": false,
                "media_type": "tv"
            }),
        )
        .await;
    resp.assert_status(StatusCode::NO_CONTENT);

    // Re-fetch to verify the update took effect
    let resp = server
        .admin_client()
        .get(&format!("/api/v1/watch-directories/{}", id))
        .await;
    resp.assert_ok();
    let updated: Value = resp.json();
    assert_eq!(updated["name"], "Updated Name");
    assert_eq!(updated["enabled"], false);
    assert_eq!(updated["media_type"], "tv");

    server.cleanup().await;
}

#[tokio::test]
async fn test_delete_watch_directory() {
    let server = TestServer::start().await;

    let resp = server
        .admin_client()
        .post_json(
            "/api/v1/watch-directories",
            &watch_dir_payload("/tmp/delete-test"),
        )
        .await;
    resp.assert_status(StatusCode::CREATED);
    let created: Value = resp.json();
    let id = created["id"].as_str().unwrap();

    let resp = server
        .admin_client()
        .delete(&format!("/api/v1/watch-directories/{}", id))
        .await;
    resp.assert_status(StatusCode::NO_CONTENT);

    // Verify it's gone
    let resp = server
        .admin_client()
        .get(&format!("/api/v1/watch-directories/{}", id))
        .await;
    resp.assert_status(StatusCode::NOT_FOUND);

    server.cleanup().await;
}

#[tokio::test]
async fn test_scan_watch_directory() {
    let server = TestServer::start().await;

    let resp = server
        .admin_client()
        .post_json(
            "/api/v1/watch-directories",
            &watch_dir_payload("/tmp/scan-test"),
        )
        .await;
    resp.assert_status(StatusCode::CREATED);
    let created: Value = resp.json();
    let id = created["id"].as_str().unwrap();

    // Trigger scan - NATS is not available in test server, so returns 503
    let resp = server
        .admin_client()
        .post(&format!("/api/v1/watch-directories/{}/scan", id))
        .await;
    resp.assert_status(StatusCode::SERVICE_UNAVAILABLE);

    server.cleanup().await;
}

#[tokio::test]
async fn test_scan_requires_auth() {
    let server = TestServer::start().await;

    let resp = server
        .admin_client()
        .post_json(
            "/api/v1/watch-directories",
            &watch_dir_payload("/tmp/scan-auth-test"),
        )
        .await;
    resp.assert_status(StatusCode::CREATED);
    let created: Value = resp.json();
    let id = created["id"].as_str().unwrap();

    let resp = server
        .anon_client()
        .post(&format!("/api/v1/watch-directories/{}/scan", id))
        .await;
    resp.assert_status(StatusCode::UNAUTHORIZED);

    server.cleanup().await;
}

#[tokio::test]
async fn test_list_files_empty() {
    let server = TestServer::start().await;

    let resp = server
        .admin_client()
        .post_json(
            "/api/v1/watch-directories",
            &watch_dir_payload("/tmp/files-test"),
        )
        .await;
    resp.assert_status(StatusCode::CREATED);
    let created: Value = resp.json();
    let id = created["id"].as_str().unwrap();

    let resp = server
        .admin_client()
        .get(&format!("/api/v1/watch-directories/{}/files", id))
        .await;
    resp.assert_ok();
    let files: Vec<Value> = resp.json();
    assert!(files.is_empty());

    server.cleanup().await;
}

#[tokio::test]
async fn test_list_files_requires_auth() {
    let server = TestServer::start().await;

    let resp = server
        .admin_client()
        .post_json(
            "/api/v1/watch-directories",
            &watch_dir_payload("/tmp/files-auth-test"),
        )
        .await;
    resp.assert_status(StatusCode::CREATED);
    let created: Value = resp.json();
    let id = created["id"].as_str().unwrap();

    let resp = server
        .anon_client()
        .get(&format!("/api/v1/watch-directories/{}/files", id))
        .await;
    resp.assert_status(StatusCode::UNAUTHORIZED);

    server.cleanup().await;
}

#[tokio::test]
async fn test_filesystem_browse() {
    let server = TestServer::start().await;

    // Browse root - should succeed
    let resp = server
        .admin_client()
        .get("/api/v1/filesystem/browse?path=/tmp")
        .await;
    resp.assert_ok();
    let json: Value = resp.json();
    assert!(json["current_path"].is_string());
    assert!(json["directories"].is_array());

    server.cleanup().await;
}

#[tokio::test]
async fn test_filesystem_browse_requires_auth() {
    let server = TestServer::start().await;

    let resp = server
        .anon_client()
        .get("/api/v1/filesystem/browse?path=/tmp")
        .await;
    resp.assert_status(StatusCode::UNAUTHORIZED);

    server.cleanup().await;
}

#[tokio::test]
async fn test_filesystem_browse_invalid_path() {
    let server = TestServer::start().await;

    let resp = server
        .admin_client()
        .get("/api/v1/filesystem/browse?path=/nonexistent/path/abc123")
        .await;
    resp.assert_status(StatusCode::BAD_REQUEST);

    server.cleanup().await;
}

#[tokio::test]
async fn test_create_all_media_types() {
    let server = TestServer::start().await;

    for (i, media_type) in ["book", "tv", "movie"].iter().enumerate() {
        let resp = server
            .admin_client()
            .post_json(
                "/api/v1/watch-directories",
                &json!({
                    "path": format!("/tmp/media-type-test-{}", i),
                    "name": format!("Test {}", media_type),
                    "media_type": media_type,
                    "callback_url": "http://localhost/ingest",
                    "recursive": true,
                    "enabled": true
                }),
            )
            .await;
        resp.assert_status(StatusCode::CREATED);
        let json: Value = resp.json();
        assert_eq!(json["media_type"], *media_type);
    }

    server.cleanup().await;
}
