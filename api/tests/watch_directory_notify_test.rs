//! Tests for the WatchDirectoryNotifyHandler job handler.
//!
//! Verifies that the handler correctly dispatches HTTP callbacks for discovered
//! files, includes auth headers, skips already-processed files, and handles
//! callback failures.

mod common;

use std::sync::{Arc, Mutex};

use axum::extract::State;
use axum::http::StatusCode as AxumStatusCode;
use axum::response::IntoResponse;
use axum::routing::post;
use axum::{Json, Router};
use serde_json::{Value, json};
use tokio::net::TcpListener;
use uuid::Uuid;

use common::TestServer;
use kobload_api::jobs::handlers::WatchDirectoryNotifyHandler;
use kobload_api::jobs::worker::JobHandler;
use kobload_api::repositories::watch_directories as wd_repo;
use tokio_util::sync::CancellationToken;

// =============================================================================
// Mock callback server
// =============================================================================

struct CallbackCapture {
    calls: Mutex<Vec<(axum::http::HeaderMap, Value)>>,
}

async fn callback_handler(
    headers: axum::http::HeaderMap,
    State(capture): State<Arc<CallbackCapture>>,
    Json(body): Json<Value>,
) -> impl IntoResponse {
    capture.calls.lock().unwrap().push((headers, body));
    AxumStatusCode::OK
}

async fn failing_callback_handler() -> impl IntoResponse {
    (AxumStatusCode::INTERNAL_SERVER_ERROR, "boom")
}

/// Start a mock HTTP server that captures callback requests.
/// Returns `(base_url, capture)`.
async fn start_mock_callback_server() -> (String, Arc<CallbackCapture>) {
    let capture = Arc::new(CallbackCapture {
        calls: Mutex::new(Vec::new()),
    });
    let app = Router::new()
        .route("/callback", post(callback_handler))
        .with_state(capture.clone());
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let mock_url = format!("http://{}", addr);
    tokio::spawn(async move { axum::serve(listener, app).await.ok() });
    (mock_url, capture)
}

/// Start a mock HTTP server that always returns 500.
async fn start_failing_callback_server() -> String {
    let app = Router::new().route("/callback", post(failing_callback_handler));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let mock_url = format!("http://{}", addr);
    tokio::spawn(async move { axum::serve(listener, app).await.ok() });
    mock_url
}

// =============================================================================
// Helpers
// =============================================================================

/// Create a watch directory pointing at the given callback URL.
async fn create_test_directory(
    pool: &sqlx::PgPool,
    callback_url: &str,
    auth_token: Option<&str>,
) -> wd_repo::WatchDirectory {
    wd_repo::create(
        pool,
        wd_repo::WatchDirectoryInput {
            path: format!("/tmp/notify-test-{}", Uuid::new_v4()),
            name: "Notify Test".to_string(),
            enabled: true,
            recursive: true,
            media_type: "book".to_string(),
            callback_url: callback_url.to_string(),
            auth_token: auth_token.map(String::from),
        },
    )
    .await
    .expect("create watch directory")
}

/// Insert a discovered file into the given directory.
async fn insert_discovered_file(pool: &sqlx::PgPool, dir_id: Uuid) -> wd_repo::WatchDirectoryFile {
    wd_repo::upsert_file(
        pool,
        wd_repo::WatchDirectoryFileInput {
            watch_directory_id: dir_id,
            file_path: format!("/tmp/notify-test/book-{}.epub", Uuid::new_v4()),
            file_name: "book.epub".to_string(),
            file_size: 1024,
            file_hash: Some("abc123".to_string()),
            file_mtime: None,
            interpretation: Some(json!({"title": "Test Book", "author": "Author"})),
        },
    )
    .await
    .expect("upsert file")
}

/// Run the notify handler for a given directory.
async fn run_handler(server: &TestServer, dir_id: Uuid) -> Value {
    let handler = WatchDirectoryNotifyHandler::new(&server.app_state);
    handler
        .execute(
            Uuid::new_v4(),
            json!({ "watch_directory_id": dir_id }),
            CancellationToken::new(),
        )
        .await
        .expect("handler execute")
}

// =============================================================================
// Tests
// =============================================================================

#[tokio::test]
async fn test_notify_sends_callback() {
    let server = TestServer::start().await;
    let (mock_url, capture) = start_mock_callback_server().await;

    let dir = create_test_directory(&server.pool, &format!("{}/callback", mock_url), None).await;
    let file = insert_discovered_file(&server.pool, dir.id).await;

    let result = run_handler(&server, dir.id).await;

    assert_eq!(result["sent"], 1);
    assert_eq!(result["errors"], 0);

    // Verify the callback was received
    {
        let calls = capture.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);

        let (_headers, body) = &calls[0];
        assert_eq!(body["source_type"], "watch_directory");
        assert_eq!(body["file_id"], file.id.to_string());
        assert_eq!(body["media_type"], "book");
        assert_eq!(
            body["release_interpretation"],
            json!({"title": "Test Book", "author": "Author"})
        );

        // file_url should contain the server's public_base_url
        let file_url = body["file_url"].as_str().unwrap();
        assert!(
            file_url.starts_with(&server.app_state.public_base_url),
            "file_url should start with public_base_url: {}",
            file_url
        );
        assert!(file_url.contains(&file.id.to_string()));
    }

    // Verify file status changed to sent in DB
    let updated = wd_repo::get_file_by_id(&server.pool, file.id)
        .await
        .unwrap()
        .expect("file should exist");
    assert_eq!(updated.status, "sent");
    assert!(updated.callback_sent_at.is_some());

    server.cleanup().await;
}

#[tokio::test]
async fn test_notify_includes_auth_header() {
    let server = TestServer::start().await;
    let (mock_url, capture) = start_mock_callback_server().await;

    let dir = create_test_directory(
        &server.pool,
        &format!("{}/callback", mock_url),
        Some("secret-token"),
    )
    .await;
    insert_discovered_file(&server.pool, dir.id).await;

    let result = run_handler(&server, dir.id).await;
    assert_eq!(result["sent"], 1);

    {
        let calls = capture.calls.lock().unwrap();
        assert_eq!(calls.len(), 1);

        let (headers, _body) = &calls[0];
        let auth = headers
            .get("authorization")
            .expect("Authorization header should be present")
            .to_str()
            .unwrap();
        assert_eq!(auth, "Bearer secret-token");
    }

    server.cleanup().await;
}

#[tokio::test]
async fn test_notify_skips_already_sent() {
    let server = TestServer::start().await;
    let (mock_url, capture) = start_mock_callback_server().await;

    let dir = create_test_directory(&server.pool, &format!("{}/callback", mock_url), None).await;
    let file = insert_discovered_file(&server.pool, dir.id).await;

    // Mark file as already sent
    wd_repo::mark_file_sent(&server.pool, file.id)
        .await
        .unwrap();

    let result = run_handler(&server, dir.id).await;

    assert_eq!(result["sent"], 0);
    assert_eq!(result["errors"], 0);

    // No callback should have been made
    assert_eq!(capture.calls.lock().unwrap().len(), 0);

    server.cleanup().await;
}

#[tokio::test]
async fn test_notify_skips_skipped_files() {
    let server = TestServer::start().await;
    let (mock_url, capture) = start_mock_callback_server().await;

    let dir = create_test_directory(&server.pool, &format!("{}/callback", mock_url), None).await;
    let file = insert_discovered_file(&server.pool, dir.id).await;

    // Mark file as skipped
    wd_repo::mark_file_skipped(&server.pool, file.id, None)
        .await
        .unwrap();

    let result = run_handler(&server, dir.id).await;

    assert_eq!(result["sent"], 0);
    assert_eq!(result["errors"], 0);

    assert_eq!(capture.calls.lock().unwrap().len(), 0);

    server.cleanup().await;
}

#[tokio::test]
async fn test_notify_marks_error_on_failure() {
    let server = TestServer::start().await;
    let mock_url = start_failing_callback_server().await;

    let dir = create_test_directory(&server.pool, &format!("{}/callback", mock_url), None).await;
    let file = insert_discovered_file(&server.pool, dir.id).await;

    let result = run_handler(&server, dir.id).await;

    assert_eq!(result["sent"], 0);
    assert_eq!(result["errors"], 1);

    // Verify file status is error in DB
    let updated = wd_repo::get_file_by_id(&server.pool, file.id)
        .await
        .unwrap()
        .expect("file should exist");
    assert_eq!(updated.status, "error");
    assert!(
        updated.callback_error.is_some(),
        "callback_error should be set"
    );
    let error_msg = updated.callback_error.unwrap();
    assert!(
        error_msg.contains("500"),
        "error should mention HTTP status: {}",
        error_msg
    );

    server.cleanup().await;
}

#[tokio::test]
async fn test_notify_empty_directory() {
    let server = TestServer::start().await;
    let (mock_url, capture) = start_mock_callback_server().await;

    let dir = create_test_directory(&server.pool, &format!("{}/callback", mock_url), None).await;

    // No files inserted - directory is empty
    let result = run_handler(&server, dir.id).await;

    assert_eq!(result["sent"], 0);
    assert_eq!(result["errors"], 0);

    assert_eq!(capture.calls.lock().unwrap().len(), 0);

    server.cleanup().await;
}
