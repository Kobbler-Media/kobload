//! Tests for the WatchDirectoryScanHandler job handler.
//!
//! Verifies directory scanning, file discovery, mtime dedup, recursive vs
//! non-recursive traversal, extension filtering, and hidden file skipping.

mod common;

use common::TestServer;
use kobload_api::jobs::handlers::WatchDirectoryScanHandler;
use kobload_api::jobs::worker::JobHandler;
use kobload_api::nats::NatsState;
use kobload_api::repositories::watch_directories as wd_repo;
use serde_json::json;
use sqlx::PgPool;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

// =============================================================================
// Helpers
// =============================================================================

/// Create a watch directory in the DB pointing at the given path.
async fn create_watch_dir(
    pool: &PgPool,
    path: &str,
    media_type: &str,
    recursive: bool,
) -> wd_repo::WatchDirectory {
    wd_repo::create(
        pool,
        wd_repo::WatchDirectoryInput {
            path: path.to_string(),
            name: "Test Dir".to_string(),
            enabled: true,
            recursive,
            media_type: media_type.to_string(),
            callback_url: "http://localhost/callback".to_string(),
            auth_token: None,
        },
    )
    .await
    .expect("failed to create watch directory")
}

/// Build a handler with no NATS connection.
fn build_handler(pool: &PgPool) -> WatchDirectoryScanHandler {
    WatchDirectoryScanHandler::new(pool.clone(), NatsState::new(None))
}

/// Run the scan handler for a watch directory, returning the result JSON.
async fn run_scan(
    handler: &WatchDirectoryScanHandler,
    dir_id: Uuid,
) -> Result<serde_json::Value, kobload_api::jobs::JobError> {
    handler
        .execute(
            Uuid::new_v4(),
            json!({ "watch_directory_id": dir_id }),
            CancellationToken::new(),
        )
        .await
}

/// Create a file with some content in the given directory.
fn create_file(dir: &std::path::Path, name: &str) {
    let path = dir.join(name);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("failed to create parent dirs");
    }
    std::fs::write(&path, "test content").expect("failed to write test file");
}

// =============================================================================
// Tests
// =============================================================================

#[tokio::test]
async fn test_scan_discovers_book_files() {
    let server = TestServer::start().await;
    let tmp = TempDir::new().unwrap();

    create_file(tmp.path(), "Book Title.epub");
    create_file(tmp.path(), "Another Book.pdf");

    let dir = create_watch_dir(&server.pool, &tmp.path().to_string_lossy(), "book", true).await;
    let handler = build_handler(&server.pool);

    let result = run_scan(&handler, dir.id)
        .await
        .expect("scan should succeed");
    assert_eq!(result["discovered"].as_i64(), Some(2));
    assert_eq!(result["total_files"].as_i64(), Some(2));
    assert_eq!(result["skipped_existing"].as_i64(), Some(0));

    let files = wd_repo::list_files(&server.pool, dir.id, 100, 0)
        .await
        .unwrap();
    assert_eq!(files.len(), 2);
    for f in &files {
        assert_eq!(f.status, "discovered");
        assert!(f.file_size > 0);
        assert!(f.interpretation.is_some());
    }

    server.cleanup().await;
}

#[tokio::test]
async fn test_scan_discovers_video_files() {
    let server = TestServer::start().await;
    let tmp = TempDir::new().unwrap();

    create_file(tmp.path(), "Show.S01E01.720p.mkv");
    create_file(tmp.path(), "Movie.2024.mp4");

    let dir = create_watch_dir(&server.pool, &tmp.path().to_string_lossy(), "tv", true).await;
    let handler = build_handler(&server.pool);

    let result = run_scan(&handler, dir.id)
        .await
        .expect("scan should succeed");
    assert_eq!(result["discovered"].as_i64(), Some(2));
    assert_eq!(result["total_files"].as_i64(), Some(2));

    let files = wd_repo::list_files(&server.pool, dir.id, 100, 0)
        .await
        .unwrap();
    assert_eq!(files.len(), 2);
    for f in &files {
        assert_eq!(f.status, "discovered");
    }

    server.cleanup().await;
}

#[tokio::test]
async fn test_scan_skips_existing_same_mtime() {
    let server = TestServer::start().await;
    let tmp = TempDir::new().unwrap();

    create_file(tmp.path(), "Existing.epub");

    let dir = create_watch_dir(&server.pool, &tmp.path().to_string_lossy(), "book", true).await;
    let handler = build_handler(&server.pool);

    // First scan discovers the file.
    let result = run_scan(&handler, dir.id).await.expect("first scan");
    assert_eq!(result["discovered"].as_i64(), Some(1));
    assert_eq!(result["skipped_existing"].as_i64(), Some(0));

    // Second scan should skip it (mtime unchanged).
    let result = run_scan(&handler, dir.id).await.expect("second scan");
    assert_eq!(result["discovered"].as_i64(), Some(0));
    assert_eq!(result["skipped_existing"].as_i64(), Some(1));
    assert_eq!(result["total_files"].as_i64(), Some(1));

    server.cleanup().await;
}

#[tokio::test]
async fn test_scan_non_recursive() {
    let server = TestServer::start().await;
    let tmp = TempDir::new().unwrap();

    // File in root should be found.
    create_file(tmp.path(), "Root.epub");
    // File in subdirectory should NOT be found.
    create_file(tmp.path(), "sub/Nested.epub");

    let dir = create_watch_dir(&server.pool, &tmp.path().to_string_lossy(), "book", false).await;
    let handler = build_handler(&server.pool);

    let result = run_scan(&handler, dir.id)
        .await
        .expect("scan should succeed");
    assert_eq!(result["discovered"].as_i64(), Some(1));
    assert_eq!(result["total_files"].as_i64(), Some(1));

    let files = wd_repo::list_files(&server.pool, dir.id, 100, 0)
        .await
        .unwrap();
    assert_eq!(files.len(), 1);
    assert!(files[0].file_name.contains("Root"));

    server.cleanup().await;
}

#[tokio::test]
async fn test_scan_recursive() {
    let server = TestServer::start().await;
    let tmp = TempDir::new().unwrap();

    create_file(tmp.path(), "Root.epub");
    create_file(tmp.path(), "sub/Nested.epub");
    create_file(tmp.path(), "sub/deep/DeepNested.pdf");

    let dir = create_watch_dir(&server.pool, &tmp.path().to_string_lossy(), "book", true).await;
    let handler = build_handler(&server.pool);

    let result = run_scan(&handler, dir.id)
        .await
        .expect("scan should succeed");
    assert_eq!(result["discovered"].as_i64(), Some(3));
    assert_eq!(result["total_files"].as_i64(), Some(3));

    let files = wd_repo::list_files(&server.pool, dir.id, 100, 0)
        .await
        .unwrap();
    assert_eq!(files.len(), 3);

    server.cleanup().await;
}

#[tokio::test]
async fn test_scan_missing_directory() {
    let server = TestServer::start().await;

    let dir = create_watch_dir(
        &server.pool,
        "/tmp/nonexistent_watch_dir_test_12345",
        "book",
        true,
    )
    .await;
    let handler = build_handler(&server.pool);

    let result = run_scan(&handler, dir.id).await;
    assert!(result.is_err(), "scan of missing directory should fail");

    let err = result.unwrap_err().to_string();
    assert!(
        err.contains("does not exist"),
        "error should mention missing directory, got: {}",
        err,
    );

    server.cleanup().await;
}

#[tokio::test]
async fn test_scan_ignores_wrong_extensions() {
    let server = TestServer::start().await;
    let tmp = TempDir::new().unwrap();

    create_file(tmp.path(), "notes.txt");
    create_file(tmp.path(), "photo.jpg");
    create_file(tmp.path(), "data.csv");

    let dir = create_watch_dir(&server.pool, &tmp.path().to_string_lossy(), "book", true).await;
    let handler = build_handler(&server.pool);

    let result = run_scan(&handler, dir.id)
        .await
        .expect("scan should succeed");
    assert_eq!(result["discovered"].as_i64(), Some(0));
    assert_eq!(result["total_files"].as_i64(), Some(0));

    let files = wd_repo::list_files(&server.pool, dir.id, 100, 0)
        .await
        .unwrap();
    assert!(files.is_empty());

    server.cleanup().await;
}

#[tokio::test]
async fn test_scan_ignores_hidden_files() {
    let server = TestServer::start().await;
    let tmp = TempDir::new().unwrap();

    create_file(tmp.path(), ".hidden.epub");
    create_file(tmp.path(), "visible.epub");

    let dir = create_watch_dir(&server.pool, &tmp.path().to_string_lossy(), "book", true).await;
    let handler = build_handler(&server.pool);

    let result = run_scan(&handler, dir.id)
        .await
        .expect("scan should succeed");
    assert_eq!(result["discovered"].as_i64(), Some(1));
    assert_eq!(result["total_files"].as_i64(), Some(1));

    let files = wd_repo::list_files(&server.pool, dir.id, 100, 0)
        .await
        .unwrap();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].file_name, "visible.epub");

    server.cleanup().await;
}
