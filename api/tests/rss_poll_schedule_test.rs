//! `last_check_at` is the RSS poll timer. Searches used to bump it too, so a
//! heavily searched indexer (NZBgeek: ~200 API calls a day) kept looking freshly
//! polled and missed RSS cycles, polling every 20–80 minutes instead of 15.

mod common;

use serde_json::json;
use time::OffsetDateTime;

use common::TestServer;
use kobload_api::repositories::watch as watch_repo;
use uuid::Uuid;

async fn create_source(pool: &sqlx::PgPool, name: &str) -> Uuid {
    watch_repo::create_indexer_source(
        pool,
        watch_repo::IndexerSourceInput {
            name: name.to_string(),
            handler_type: "newznab".to_string(),
            handler_config: json!({ "base_url": "http://test", "poll_interval_minutes": 15 }),
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

/// An RSS poll 20 minutes ago, so the source is due again.
async fn polled_20_minutes_ago(pool: &sqlx::PgPool, id: Uuid) -> OffsetDateTime {
    watch_repo::update_indexer_source_state(pool, id, None, None)
        .await
        .expect("rss poll");
    sqlx::query_scalar::<_, OffsetDateTime>(
        "UPDATE indexer_sources SET last_check_at = NOW() - INTERVAL '20 minutes' \
         WHERE id = $1 RETURNING last_check_at",
    )
    .bind(id)
    .fetch_one(pool)
    .await
    .expect("backdate poll")
}

async fn last_check_at(pool: &sqlx::PgPool, id: Uuid) -> Option<OffsetDateTime> {
    watch_repo::get_indexer_source_by_id(pool, id)
        .await
        .expect("query")
        .expect("source exists")
        .last_check_at
}

async fn is_due(pool: &sqlx::PgPool, id: Uuid) -> bool {
    watch_repo::get_sources_to_check(pool)
        .await
        .expect("due sources")
        .iter()
        .any(|s| s.id == id)
}

#[tokio::test]
async fn searches_do_not_delay_the_next_rss_poll() {
    let server = TestServer::start().await;
    let pool = &server.pool;
    let id = create_source(pool, "Searched indexer").await;
    let polled = polled_20_minutes_ago(pool, id).await;
    assert!(is_due(pool, id).await);

    watch_repo::record_success(pool, id)
        .await
        .expect("search ok");
    assert_eq!(last_check_at(pool, id).await, Some(polled));
    assert!(
        is_due(pool, id).await,
        "a successful search must not reset the RSS timer"
    );

    watch_repo::record_failure(pool, id, "search timed out")
        .await
        .expect("search failed");
    assert_eq!(last_check_at(pool, id).await, Some(polled));

    watch_repo::record_auth_failure(pool, id, "401")
        .await
        .expect("search auth failure");
    assert_eq!(last_check_at(pool, id).await, Some(polled));

    // Rate limiting is enforced through rate_limited_until, not the poll timer.
    watch_repo::record_rate_limited(pool, id, Some(60))
        .await
        .expect("search rate limited");
    let row = watch_repo::get_indexer_source_by_id(pool, id)
        .await
        .expect("query")
        .expect("source exists");
    assert_eq!(row.last_check_at, Some(polled));
    assert!(row.rate_limited_until.is_some());
    assert!(watch_repo::should_skip_source(&row));

    server.cleanup().await;
}

#[tokio::test]
async fn an_rss_poll_resets_the_timer() {
    let server = TestServer::start().await;
    let pool = &server.pool;
    let id = create_source(pool, "Polled indexer").await;
    let polled = polled_20_minutes_ago(pool, id).await;

    watch_repo::update_indexer_source_state(pool, id, None, None)
        .await
        .expect("rss poll");
    assert!(last_check_at(pool, id).await.expect("set") > polled);
    assert!(!is_due(pool, id).await);

    // A failed poll also counts as a poll.
    polled_20_minutes_ago(pool, id).await;
    watch_repo::update_indexer_source_state(pool, id, None, Some("connection reset"))
        .await
        .expect("failed rss poll");
    assert!(!is_due(pool, id).await);

    server.cleanup().await;
}
