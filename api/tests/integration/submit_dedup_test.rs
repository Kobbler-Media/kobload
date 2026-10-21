//! Submit-time duplicate-URL dedup (bug #2): two kobbler watch patterns firing
//! for the same release ~1s apart, or a later `want` re-grabbing a release that
//! already imported, must not produce duplicate downloads. `submit_download`
//! suppresses a submission when `skip_duplicate_url` is set and a recent active
//! download already exists for the same URL.

use crate::common;

use std::sync::Arc;

use uuid::Uuid;

use common::{
    MockDownloadClient, MockDownloadClientResolver, MockIndexer, MockIndexerResolver, TestServer,
    TestServerOptions,
};
use kobload_api::download_clients::DownloadClientError;
use kobload_api::repositories::{clients as clients_repo, downloads as downloads_repo};
use kobload_api::services::downloads::{
    DownloadServiceContext, DownloadSubmission, DownloadUrlContext, submit_download,
};

const MAGNET: &str = "magnet:?xt=urn:btih:aaf840d62ec159122abc0804c18324768b807b80&dn=Moto3";

async fn start() -> (TestServer, Arc<MockDownloadClient>) {
    let mock_dl = Arc::new(MockDownloadClient::new());
    let mock_idx = Arc::new(MockIndexer::new());
    let options = TestServerOptions {
        download_clients: Arc::new(MockDownloadClientResolver::new(mock_dl.clone())),
        indexers: Arc::new(MockIndexerResolver::new(mock_idx.clone())),
    };
    let server = TestServer::start_with_options(options).await;
    clients_repo::create_client(
        &server.pool,
        clients_repo::DownloadClientInput {
            name: "Mock Torrent Client".to_string(),
            client_type: "mock".to_string(),
            protocol: "torrent".to_string(),
            base_url: "http://localhost:9999".to_string(),
            is_default: true,
            enabled: true,
            ..Default::default()
        },
    )
    .await
    .expect("create download client");
    (server, mock_dl)
}

fn submission(skip_duplicate_url: bool) -> DownloadSubmission {
    DownloadSubmission {
        source: "watch".to_string(),
        protocol: "torrent".to_string(),
        name: "Moto3 2026 Hungary Race".to_string(),
        download_url: MAGNET.to_string(),
        download_client_id: None,
        callback_url: Some("http://localhost/callback".to_string()),
        auth_token: None,
        metadata: serde_json::json!({}),
        release_payload: None,
        api_key_id: None,
        url_context: DownloadUrlContext::default(),
        indexer_source_id: None,
        quality_profile_id: None,
        media_kind: None,
        skip_duplicate_url,
    }
}

fn ctx(server: &TestServer) -> DownloadServiceContext<'_> {
    DownloadServiceContext {
        pool: &server.pool,
        http_client: &server.app_state.http_client,
        download_events: &server.app_state.download_events,
        download_clients: server.app_state.download_clients.as_ref(),
        indexers: server.app_state.indexers.as_ref(),
    }
}

async fn count_for_url(pool: &sqlx::PgPool, url: &str) -> i64 {
    sqlx::query_scalar!(
        "SELECT COUNT(*) FROM downloads WHERE download_url = $1",
        url
    )
    .fetch_one(pool)
    .await
    .unwrap()
    .unwrap_or(0)
}

#[tokio::test]
async fn second_submission_same_url_is_suppressed() {
    let (server, _mock) = start().await;
    let c = ctx(&server);

    let first = submit_download(&c, submission(true))
        .await
        .expect("first submit");
    let second = submit_download(&c, submission(true))
        .await
        .expect("second submit");

    assert_eq!(
        first.id, second.id,
        "duplicate submission must return the existing download id"
    );
    assert_eq!(
        count_for_url(&server.pool, MAGNET).await,
        1,
        "only one download row should exist for the URL"
    );
}

#[tokio::test]
async fn flag_off_allows_duplicate() {
    let (server, _mock) = start().await;
    let c = ctx(&server);

    let first = submit_download(&c, submission(false))
        .await
        .expect("first submit");
    let second = submit_download(&c, submission(false))
        .await
        .expect("second submit");

    assert_ne!(
        first.id, second.id,
        "with dedup off (manual download), a second submit creates a new row"
    );
    assert_eq!(count_for_url(&server.pool, MAGNET).await, 2);
}

#[tokio::test]
async fn failed_prior_does_not_suppress_retry() {
    let (server, mock) = start().await;
    let c = ctx(&server);

    // First submit fails permanently -> status "failed".
    mock.on_add_url(Err(DownloadClientError::Message("boom".to_string())));
    let first = submit_download(&c, submission(true))
        .await
        .expect("first submit");

    // Second submit (mock now succeeds) must NOT be suppressed by the failed row.
    let second = submit_download(&c, submission(true))
        .await
        .expect("second submit");

    assert_ne!(
        first.id, second.id,
        "a failed prior download must not suppress a retry"
    );
    assert_eq!(count_for_url(&server.pool, MAGNET).await, 2);
}

#[tokio::test]
async fn dedup_query_respects_time_window() {
    let (server, _mock) = start().await;
    let c = ctx(&server);

    // Create one active download for the URL.
    submit_download(&c, submission(true)).await.expect("submit");

    // Within the window the row is found; with a zero-hour window the row (just
    // created) falls outside `created_at > NOW() - interval '0h'` and is ignored,
    // proving the window bound is actually applied rather than always matching.
    let in_window = downloads_repo::find_recent_active_download_by_url(&server.pool, MAGNET, 48)
        .await
        .expect("query");
    let out_of_window = downloads_repo::find_recent_active_download_by_url(&server.pool, MAGNET, 0)
        .await
        .expect("query");

    assert!(
        in_window.is_some(),
        "recent download must match a 48h window"
    );
    assert!(
        out_of_window.is_none(),
        "a zero-hour window must exclude even a just-created download"
    );
}

fn reservation_input(client_id: Uuid) -> downloads_repo::DownloadInput {
    downloads_repo::DownloadInput {
        source: "watch".to_string(),
        protocol: "torrent".to_string(),
        profile_id: None,
        name: "Moto3 2026 Hungary Race".to_string(),
        download_url: MAGNET.to_string(),
        download_client_id: client_id,
        external_id: None,
        callback_url: Some("http://localhost/callback".to_string()),
        auth_token: None,
        metadata: Some(serde_json::json!({})),
        status: "submitting".to_string(),
        progress: None,
        error: None,
        release_payload: None,
        api_key_id: None,
        quality_profile_id: None,
        media_kind: None,
        submission_error: None,
    }
}

async fn default_client(pool: &sqlx::PgPool) -> Uuid {
    clients_repo::create_client(
        pool,
        clients_repo::DownloadClientInput {
            name: "Mock".to_string(),
            client_type: "mock".to_string(),
            protocol: "torrent".to_string(),
            base_url: "http://localhost:9999".to_string(),
            is_default: true,
            enabled: true,
            ..Default::default()
        },
    )
    .await
    .expect("client")
}

#[tokio::test]
async fn reserve_blocks_second_active_same_key() {
    let server = TestServer::start().await;
    let client_id = default_client(&server.pool).await;

    let first =
        downloads_repo::reserve_download(&server.pool, reservation_input(client_id), Some("k"))
            .await
            .expect("reserve");
    let second =
        downloads_repo::reserve_download(&server.pool, reservation_input(client_id), Some("k"))
            .await
            .expect("reserve");

    assert!(first.is_some(), "first reservation wins");
    assert!(
        second.is_none(),
        "second in-flight reservation for the same dedup_key is rejected by the index"
    );
}

#[tokio::test]
async fn reserve_null_key_never_conflicts() {
    let server = TestServer::start().await;
    let client_id = default_client(&server.pool).await;

    let a = downloads_repo::reserve_download(&server.pool, reservation_input(client_id), None)
        .await
        .expect("reserve");
    let b = downloads_repo::reserve_download(&server.pool, reservation_input(client_id), None)
        .await
        .expect("reserve");

    assert!(
        a.is_some() && b.is_some(),
        "NULL dedup_key (manual) never conflicts"
    );
    assert_ne!(a, b);
}

#[tokio::test]
async fn reserve_key_released_after_terminal_status() {
    let server = TestServer::start().await;
    let client_id = default_client(&server.pool).await;

    let first =
        downloads_repo::reserve_download(&server.pool, reservation_input(client_id), Some("k"))
            .await
            .expect("reserve")
            .expect("first wins");
    // Finalize to a terminal status -> index predicate no longer covers the row.
    downloads_repo::finalize_submission(
        &server.pool,
        first,
        "failed",
        None,
        Some("boom"),
        None,
        None,
        MAGNET,
    )
    .await
    .expect("finalize");

    let second =
        downloads_repo::reserve_download(&server.pool, reservation_input(client_id), Some("k"))
            .await
            .expect("reserve");
    assert!(
        second.is_some(),
        "a failed/terminal prior reservation releases the key for a retry"
    );
}
