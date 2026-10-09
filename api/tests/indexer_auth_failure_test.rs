//! Credential rejections must be distinguishable from transient indexer failures:
//! a dead MAM/TorrentDay cookie cannot be fixed by retrying, so it gets its own
//! state rather than disappearing into the failure count.

mod common;

use serde_json::json;

use common::TestServer;
use kobload_api::indexers::IndexerError;
use kobload_api::repositories::watch as watch_repo;
use uuid::Uuid;

async fn create_source(pool: &sqlx::PgPool, name: &str) -> Uuid {
    watch_repo::create_indexer_source(
        pool,
        watch_repo::IndexerSourceInput {
            name: name.to_string(),
            handler_type: "mam".to_string(),
            handler_config: json!({ "mam_id": "stale-cookie" }),
            category_ids: None,
            media_type_ids: vec![],
            enabled: true,
            priority: 0,
            seed_ratio: None,
            seed_time_minutes: None,
            search_mode: None,
            auto_throttle: None,
            api_limit_daily: None,
            debrid_mode: None,
        },
    )
    .await
    .expect("create source")
}

async fn fetch(pool: &sqlx::PgPool, id: Uuid) -> watch_repo::IndexerSourceRow {
    watch_repo::get_indexer_source_by_id(pool, id)
        .await
        .expect("query")
        .expect("source exists")
}

#[tokio::test]
async fn auth_failure_is_recorded_separately_from_transient_failures() {
    let server = TestServer::start().await;
    let id = create_source(&server.pool, "MAM auth").await;

    watch_repo::record_failure(&server.pool, id, "connection reset")
        .await
        .expect("transient failure");
    let after_transient = fetch(&server.pool, id).await;
    assert_eq!(after_transient.consecutive_failures, 1);
    assert!(
        after_transient.auth_failed_at.is_none(),
        "a transient failure must not look like a credential problem"
    );

    watch_repo::record_auth_failure(&server.pool, id, "MAM returned 403")
        .await
        .expect("auth failure");
    let after_auth = fetch(&server.pool, id).await;
    assert!(after_auth.auth_failed_at.is_some());
    assert_eq!(
        after_auth.consecutive_failures, 1,
        "auth failures pin the count instead of escalating backoff"
    );
    assert_eq!(after_auth.last_error.as_deref(), Some("MAM returned 403"));
}

#[tokio::test]
async fn auth_failure_survives_later_transient_failures() {
    let server = TestServer::start().await;
    let id = create_source(&server.pool, "MAM sticky").await;

    watch_repo::record_auth_failure(&server.pool, id, "MAM returned 403")
        .await
        .expect("auth failure");
    let first = fetch(&server.pool, id).await;
    let marked_at = first.auth_failed_at.expect("marked");

    watch_repo::record_failure(&server.pool, id, "timeout")
        .await
        .expect("transient failure");
    let after = fetch(&server.pool, id).await;
    assert_eq!(
        after.auth_failed_at,
        Some(marked_at),
        "the original rejection time is kept — the credential is still bad"
    );
}

#[tokio::test]
async fn success_and_reset_both_clear_the_auth_marker() {
    let server = TestServer::start().await;

    let id = create_source(&server.pool, "MAM recovers").await;
    watch_repo::record_auth_failure(&server.pool, id, "MAM returned 403")
        .await
        .expect("auth failure");
    watch_repo::record_success(&server.pool, id)
        .await
        .expect("success");
    assert!(fetch(&server.pool, id).await.auth_failed_at.is_none());

    let id = create_source(&server.pool, "MAM reset").await;
    watch_repo::record_auth_failure(&server.pool, id, "MAM returned 403")
        .await
        .expect("auth failure");
    watch_repo::reset_indexer_health(&server.pool, id)
        .await
        .expect("reset");
    assert!(fetch(&server.pool, id).await.auth_failed_at.is_none());
}

#[tokio::test]
async fn rejected_credentials_classify_as_auth_failed() {
    let unauthorized = http_error(reqwest::StatusCode::UNAUTHORIZED).await;
    assert!(unauthorized.auth_failed);

    let forbidden = http_error(reqwest::StatusCode::FORBIDDEN).await;
    assert!(forbidden.auth_failed);

    // A broken indexer is still just a broken indexer.
    let server_error = http_error(reqwest::StatusCode::INTERNAL_SERVER_ERROR).await;
    assert!(!server_error.auth_failed);

    assert!(!IndexerError::rate_limited(Some(60)).auth_failed);
}

/// Build an `IndexerError` from a real response with the given status.
async fn http_error(status: reqwest::StatusCode) -> IndexerError {
    let response = http::Response::builder()
        .status(status)
        .header("content-type", "text/html")
        .body("<html>login</html>")
        .expect("response");
    IndexerError::bad_status("MAM", reqwest::Response::from(response)).await
}
