//! Indexer RSS cache API tests.

mod common;

use serde_json::json;

use common::TestServer;
use kobload_api::repositories::indexer_rss_cache as cache_repo;
use kobload_api::repositories::watch as watch_repo;

#[tokio::test]
async fn test_indexer_rss_cache_info() {
    let server = TestServer::start().await;

    let source_id = watch_repo::create_indexer_source(
        &server.pool,
        watch_repo::IndexerSourceInput {
            debrid_mode: None,
            name: "Cache Source".to_string(),
            handler_type: "newznab".to_string(),
            handler_config: json!({ "base_url": "http://example.com" }),
            category_ids: None,
            media_type_ids: vec![],
            enabled: true,
            priority: 0,
            seed_ratio: None,
            seed_time_minutes: None,
            search_mode: None,
            auto_throttle: None,
            api_limit_daily: None,
        },
    )
    .await
    .expect("create source");

    cache_repo::upsert_rss_cache(
        &server.pool,
        source_id,
        time::OffsetDateTime::now_utc(),
        120,
        "<rss><channel></channel></rss>",
    )
    .await
    .expect("cache");

    let response = server
        .admin_client()
        .get(&format!("/api/v1/admin/indexer-rss-cache/{source_id}"))
        .await;
    response.assert_ok();
    let body: serde_json::Value = response.json();
    assert_eq!(body["indexer_source_id"], source_id.to_string());
    assert_eq!(body["ttl_seconds"], 120);
    assert!(body["size_bytes"].as_u64().unwrap_or(0) > 0);

    server.cleanup().await;
}
