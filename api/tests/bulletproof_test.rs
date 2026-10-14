//! Tests for bulletproof plan features:
//! - Submission retry (pending_retry status)
//! - Watch callback retry
//! - Match deduplication (race condition prevention)
//! - Idempotency key generation
//! - Search cache
//! - Rate limiting unified

mod common;

use serde_json::json;
use uuid::Uuid;

use common::TestServer;
use kobload_api::repositories::{downloads as downloads_repo, watch as watch_repo};

// =============================================================================
// Download Submission Retry Tests
// =============================================================================

#[tokio::test]
async fn test_fetch_pending_retry_downloads_query() {
    let server = TestServer::start().await;
    let pool = &server.pool;

    // Create a download client first (required for download creation)
    let client_id: Uuid =
        sqlx::query_scalar("INSERT INTO download_clients (id, name, client_type, protocol, base_url, enabled, is_default) VALUES ($1, 'Test', 'mock', 'usenet', 'http://test', true, true) RETURNING id")
            .bind(Uuid::now_v7())
            .fetch_one(pool)
            .await
            .expect("create client");

    // Create a download in pending_retry status
    let download_id = downloads_repo::create_download(
        pool,
        downloads_repo::DownloadInput {
            source: "test".to_string(),
            protocol: "usenet".to_string(),
            name: "Test Download".to_string(),
            download_url: "http://example.com/test.nzb".to_string(),
            download_client_id: client_id,
            callback_url: Some("http://localhost/callback".to_string()),
            metadata: Some(json!({})),
            status: "pending_retry".to_string(),
            ..Default::default()
        },
    )
    .await
    .expect("create download");

    // Set submission_attempts to a retryable value and make the last attempt old enough
    sqlx::query("UPDATE downloads SET submission_attempts = 3, submission_last_attempt_at = NOW() - INTERVAL '10 minutes' WHERE id = $1")
        .bind(download_id)
        .execute(pool)
        .await
        .expect("update attempts");

    // Fetch pending retry downloads
    let pending = downloads_repo::fetch_pending_retry_downloads(pool)
        .await
        .expect("fetch pending retry");

    assert!(!pending.is_empty(), "should find pending_retry download");
    assert!(
        pending.iter().any(|d| d.id == download_id),
        "should include our test download"
    );

    server.cleanup().await;
}

#[tokio::test]
async fn test_pending_retry_excluded_after_max_attempts() {
    let server = TestServer::start().await;
    let pool = &server.pool;

    let client_id: Uuid =
        sqlx::query_scalar("INSERT INTO download_clients (id, name, client_type, protocol, base_url, enabled, is_default) VALUES ($1, 'Test', 'mock', 'usenet', 'http://test', true, true) RETURNING id")
            .bind(Uuid::now_v7())
            .fetch_one(pool)
            .await
            .expect("create client");

    // Create a download with max attempts reached
    let download_id = downloads_repo::create_download(
        pool,
        downloads_repo::DownloadInput {
            source: "test".to_string(),
            protocol: "usenet".to_string(),
            name: "Maxed Out Download".to_string(),
            download_url: "http://example.com/maxed.nzb".to_string(),
            download_client_id: client_id,
            callback_url: Some("http://localhost/callback".to_string()),
            metadata: Some(json!({})),
            status: "pending_retry".to_string(),
            ..Default::default()
        },
    )
    .await
    .expect("create download");

    // Set attempts to 10 (max)
    sqlx::query("UPDATE downloads SET submission_attempts = 10 WHERE id = $1")
        .bind(download_id)
        .execute(pool)
        .await
        .expect("update attempts");

    let pending = downloads_repo::fetch_pending_retry_downloads(pool)
        .await
        .expect("fetch pending retry");

    assert!(
        !pending.iter().any(|d| d.id == download_id),
        "should NOT include download with max attempts"
    );

    server.cleanup().await;
}

// =============================================================================
// Match Race Condition Prevention Tests
// =============================================================================

#[tokio::test]
async fn test_create_match_if_not_exists_returns_id_on_first_insert() {
    let server = TestServer::start().await;
    let pool = &server.pool;

    // Create a watch pattern first
    let pattern_id = watch_repo::create_pattern(
        pool,
        "test",
        "race-test-1",
        watch_repo::WatchPatternInput {
            callback_url: "http://localhost/cb".to_string(),
            patterns: vec!["test".to_string()],
            media_want: None,
            match_targets: None,
            match_mode: Some("fuzzy".to_string()),
            min_score: None,
            media_type_id: None,
            auto_download: false,
            metadata: None,
            enabled: true,
            upgrade_allowed: true,
            search_not_before: None,
        },
    )
    .await
    .expect("create pattern");

    let release_data = json!({});
    let match_input = watch_repo::WatchMatchInput {
        pattern_id,
        indexer_source_id: None,
        release_guid: "race-test-guid-1",
        release_title: "Race Test Title",
        release_data: Some(&release_data),
        match_score: 0.9,
        matched_pattern: "test",
        action: "notified",
        download_id: None,
        hold_until: None,
        held: false,
        quality_score: None,
        rejected: false,
        rejected_at: None,
        rejected_reason: None,
        episode_key: None,
    };

    // First insert should succeed
    let result1 = watch_repo::create_match_if_not_exists(pool, &match_input)
        .await
        .expect("first insert");
    assert!(result1.is_some(), "first insert should return Some(id)");

    // Second insert with same pattern+guid should return None (conflict)
    let result2 = watch_repo::create_match_if_not_exists(pool, &match_input)
        .await
        .expect("second insert");
    assert!(
        result2.is_none(),
        "second insert should return None (conflict)"
    );

    server.cleanup().await;
}

#[tokio::test]
async fn test_create_match_if_not_exists_different_guids_both_succeed() {
    let server = TestServer::start().await;
    let pool = &server.pool;

    let pattern_id = watch_repo::create_pattern(
        pool,
        "test",
        "race-test-2",
        watch_repo::WatchPatternInput {
            callback_url: "http://localhost/cb".to_string(),
            patterns: vec!["test".to_string()],
            media_want: None,
            match_targets: None,
            match_mode: Some("fuzzy".to_string()),
            min_score: None,
            media_type_id: None,
            auto_download: false,
            metadata: None,
            enabled: true,
            upgrade_allowed: true,
            search_not_before: None,
        },
    )
    .await
    .expect("create pattern");

    let release_data = json!({});

    let input1 = watch_repo::WatchMatchInput {
        pattern_id,
        indexer_source_id: None,
        release_guid: "different-guid-a",
        release_title: "Title A",
        release_data: Some(&release_data),
        match_score: 0.9,
        matched_pattern: "test",
        action: "notified",
        download_id: None,
        hold_until: None,
        held: false,
        quality_score: None,
        rejected: false,
        rejected_at: None,
        rejected_reason: None,
        episode_key: None,
    };

    let input2 = watch_repo::WatchMatchInput {
        pattern_id,
        indexer_source_id: None,
        release_guid: "different-guid-b",
        release_title: "Title B",
        release_data: Some(&release_data),
        match_score: 0.85,
        matched_pattern: "test",
        action: "notified",
        download_id: None,
        hold_until: None,
        held: false,
        quality_score: None,
        rejected: false,
        rejected_at: None,
        rejected_reason: None,
        episode_key: None,
    };

    let result1 = watch_repo::create_match_if_not_exists(pool, &input1)
        .await
        .expect("insert guid-a");
    let result2 = watch_repo::create_match_if_not_exists(pool, &input2)
        .await
        .expect("insert guid-b");

    assert!(result1.is_some(), "guid-a should succeed");
    assert!(result2.is_some(), "guid-b should succeed (different guid)");
    assert_ne!(result1, result2, "should have different match IDs");

    server.cleanup().await;
}

// =============================================================================
// Idempotency Key Tests
// =============================================================================

#[tokio::test]
async fn test_get_or_create_idempotency_key_creates_new() {
    let server = TestServer::start().await;
    let pool = &server.pool;

    let client_id: Uuid =
        sqlx::query_scalar("INSERT INTO download_clients (id, name, client_type, protocol, base_url, enabled, is_default) VALUES ($1, 'Test', 'mock', 'usenet', 'http://test', true, true) RETURNING id")
            .bind(Uuid::now_v7())
            .fetch_one(pool)
            .await
            .expect("create client");

    let download_id = downloads_repo::create_download(
        pool,
        downloads_repo::DownloadInput {
            source: "test".to_string(),
            protocol: "usenet".to_string(),
            name: "Idempotency Test".to_string(),
            download_url: "http://example.com/idemp.nzb".to_string(),
            download_client_id: client_id,
            callback_url: Some("http://localhost/callback".to_string()),
            metadata: Some(json!({})),
            status: "downloading".to_string(),
            ..Default::default()
        },
    )
    .await
    .expect("create download");

    // First call should create a new key
    let key1 = downloads_repo::get_or_create_idempotency_key(pool, download_id)
        .await
        .expect("get key 1");

    // Second call should return the same key
    let key2 = downloads_repo::get_or_create_idempotency_key(pool, download_id)
        .await
        .expect("get key 2");

    assert_eq!(key1, key2, "idempotency key should be consistent");

    // Verify it's stored in the database
    let row: (Option<Uuid>,) =
        sqlx::query_as("SELECT callback_idempotency_key FROM downloads WHERE id = $1")
            .bind(download_id)
            .fetch_one(pool)
            .await
            .expect("fetch download");

    assert_eq!(row.0, Some(key1), "key should be persisted in DB");

    server.cleanup().await;
}

// =============================================================================
// Watch Callback Retry Tests
// =============================================================================

#[tokio::test]
async fn test_get_matches_pending_callback_query() {
    let server = TestServer::start().await;
    let pool = &server.pool;

    let pattern_id = watch_repo::create_pattern(
        pool,
        "test",
        "callback-retry-1",
        watch_repo::WatchPatternInput {
            callback_url: "http://localhost/cb".to_string(),
            patterns: vec!["test".to_string()],
            media_want: None,
            match_targets: None,
            match_mode: Some("fuzzy".to_string()),
            min_score: None,
            media_type_id: None,
            auto_download: false,
            metadata: None,
            enabled: true,
            upgrade_allowed: true,
            search_not_before: None,
        },
    )
    .await
    .expect("create pattern");

    // Create a match with failed callback
    let release_data = json!({});
    let match_id = watch_repo::create_match(
        pool,
        &watch_repo::WatchMatchInput {
            pattern_id,
            indexer_source_id: None,
            release_guid: "callback-retry-guid",
            release_title: "Callback Retry Test",
            release_data: Some(&release_data),
            match_score: 0.9,
            matched_pattern: "test",
            action: "downloaded",
            download_id: None,
            hold_until: None,
            held: false,
            quality_score: None,
            rejected: false,
            rejected_at: None,
            rejected_reason: None,
            episode_key: None,
        },
    )
    .await
    .expect("create match");

    // Mark it as having a callback error (simulating failed callback)
    sqlx::query(
        "UPDATE watch_matches SET callback_error = $1, callback_attempts = 1 WHERE id = $2",
    )
    .bind("Connection refused")
    .bind(match_id)
    .execute(pool)
    .await
    .expect("set callback error");

    let pending = watch_repo::get_matches_pending_callback(pool)
        .await
        .expect("get pending callbacks");

    assert!(
        pending.iter().any(|m| m.id == match_id),
        "should find match with callback error"
    );

    server.cleanup().await;
}

#[tokio::test]
async fn test_matches_with_callback_sent_excluded() {
    let server = TestServer::start().await;
    let pool = &server.pool;

    let pattern_id = watch_repo::create_pattern(
        pool,
        "test",
        "callback-sent-1",
        watch_repo::WatchPatternInput {
            callback_url: "http://localhost/cb".to_string(),
            patterns: vec!["test".to_string()],
            media_want: None,
            match_targets: None,
            match_mode: Some("fuzzy".to_string()),
            min_score: None,
            media_type_id: None,
            auto_download: false,
            metadata: None,
            enabled: true,
            upgrade_allowed: true,
            search_not_before: None,
        },
    )
    .await
    .expect("create pattern");

    let release_data = json!({});
    let match_id = watch_repo::create_match(
        pool,
        &watch_repo::WatchMatchInput {
            pattern_id,
            indexer_source_id: None,
            release_guid: "callback-sent-guid",
            release_title: "Callback Sent Test",
            release_data: Some(&release_data),
            match_score: 0.9,
            matched_pattern: "test",
            action: "downloaded",
            download_id: None,
            hold_until: None,
            held: false,
            quality_score: None,
            rejected: false,
            rejected_at: None,
            rejected_reason: None,
            episode_key: None,
        },
    )
    .await
    .expect("create match");

    // Mark callback as sent
    watch_repo::mark_match_callback_sent(pool, match_id)
        .await
        .expect("mark sent");

    let pending = watch_repo::get_matches_pending_callback(pool)
        .await
        .expect("get pending callbacks");

    assert!(
        !pending.iter().any(|m| m.id == match_id),
        "should NOT find match with callback_sent_at set"
    );

    server.cleanup().await;
}

// =============================================================================
// Search Cache Tests
// =============================================================================

#[tokio::test]
async fn test_search_cache_round_trip() {
    use kobload_api::repositories::search_cache;

    let server = TestServer::start().await;
    let pool = &server.pool;

    let cache_key = "test-cache-key-001";
    let cache_value = json!({
        "results": [{"title": "Test Result"}],
        "errors": []
    });

    // Put value in cache
    search_cache::put(pool, cache_key, &cache_value, 300)
        .await
        .expect("cache put");

    // Get it back
    let retrieved = search_cache::get(pool, cache_key).await.expect("cache get");

    assert!(retrieved.is_some(), "should find cached value");
    assert_eq!(retrieved.unwrap(), cache_value);

    server.cleanup().await;
}

#[tokio::test]
async fn test_search_cache_miss() {
    use kobload_api::repositories::search_cache;

    let server = TestServer::start().await;
    let pool = &server.pool;

    let result = search_cache::get(pool, "nonexistent-key")
        .await
        .expect("cache get");

    assert!(result.is_none(), "should return None for cache miss");

    server.cleanup().await;
}
