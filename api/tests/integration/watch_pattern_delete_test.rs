//! A watch pattern is removed one way, whoever asks: the patterns page's
//! delete (one or several) and the `watch_pattern_delete` job both go through
//! the bulk delete, which tells the pattern's source what it lost.

use crate::common::TestServer;

use kobload_api::repositories::watch as watch_repo;
use serde_json::json;
use uuid::Uuid;

async fn pattern(pool: &sqlx::PgPool, source_id: &str) -> Uuid {
    watch_repo::create_pattern(
        pool,
        "kobbler",
        source_id,
        watch_repo::WatchPatternInput {
            callback_url: "http://localhost/cb".to_string(),
            patterns: vec!["Northern Lights".to_string()],
            media_want: None,
            match_targets: None,
            match_mode: Some("fuzzy".to_string()),
            min_score: None,
            media_type_id: None,
            auto_download: true,
            metadata: Some(json!({ "book_id": source_id })),
            enabled: true,
            upgrade_allowed: true,
            search_not_before: None,
        },
    )
    .await
    .expect("create pattern")
}

/// What the source is told about comes from the rows actually deleted.
#[tokio::test]
async fn bulk_delete_returns_each_deleted_patterns_source() {
    let server = TestServer::start().await;
    let (a, b) = (
        pattern(&server.pool, "book-a").await,
        pattern(&server.pool, "book-b").await,
    );
    pattern(&server.pool, "book-c").await;

    let mut deleted = watch_repo::bulk_delete_patterns(&server.pool, &[a, b, Uuid::nil()])
        .await
        .expect("bulk delete");
    deleted.sort_by(|x, y| x.source_id.cmp(&y.source_id));

    let got: Vec<_> = deleted
        .iter()
        .map(|d| {
            (
                d.id,
                d.source_system.as_str(),
                d.source_id.as_str(),
                d.metadata.clone(),
            )
        })
        .collect();
    assert_eq!(
        got,
        vec![
            (a, "kobbler", "book-a", Some(json!({ "book_id": "book-a" }))),
            (b, "kobbler", "book-b", Some(json!({ "book_id": "book-b" }))),
        ]
    );
    assert!(
        watch_repo::get_pattern_by_source(&server.pool, "kobbler", "book-c")
            .await
            .unwrap()
            .is_some()
    );
}

#[tokio::test]
async fn the_patterns_page_deletes_through_the_bulk_endpoint() {
    let server = TestServer::start().await;
    let id = pattern(&server.pool, "book-a").await;

    let resp = server
        .admin_client()
        .delete("/api/v1/watch/kobbler/book-a")
        .await;
    assert!(!resp.status.is_success(), "{}", resp.text());
    assert!(
        watch_repo::get_pattern_by_source(&server.pool, "kobbler", "book-a")
            .await
            .unwrap()
            .is_some()
    );

    let resp = server
        .admin_client()
        .post_json("/api/v1/watch/bulk-delete", &json!({ "ids": [id] }))
        .await;
    assert_eq!(resp.status.as_u16(), 200, "{}", resp.text());
    assert_eq!(resp.json::<serde_json::Value>()["deleted"], 1);
    assert!(
        watch_repo::get_pattern_by_source(&server.pool, "kobbler", "book-a")
            .await
            .unwrap()
            .is_none()
    );

    let resp = server
        .admin_client()
        .post_json(
            "/api/v1/watch/sync",
            &json!({ "source_system": "kobbler", "items": [] }),
        )
        .await;
    assert!(!resp.status.is_success(), "{}", resp.text());
}
