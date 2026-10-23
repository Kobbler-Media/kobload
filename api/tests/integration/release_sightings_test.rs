//! The RSS poll keeps two weeks of what it saw, once per release, for the
//! review queue to look back over.

use serde_json::json;
use uuid::Uuid;

use crate::common::entry_points::torrent;
use crate::common::tv_watch::{TvWatch, usenet_release};
use kobload_api::jobs::handlers::CacheCleanupHandler;
use kobload_api::jobs::worker::JobHandler;
use kobload_api::watch::process_releases;

#[tokio::test]
async fn the_poll_records_each_release_once_and_cleanup_drops_old_ones() {
    let tv = TvWatch::start().await;
    let pool = &tv.server.pool;
    let mut magnet = torrent("Some.Show.S01E01.1080p.WEB.h264-GRP", 'A');
    magnet.info_hash = magnet.info_hash.map(|h| h.to_uppercase());
    let releases = [
        usenet_release("nzb-guid", "Some.Show.S01E02.1080p.WEB.h264-GRP"),
        magnet,
    ];

    for _ in 0..2 {
        process_releases(&tv.server.app_state, &tv.source, &releases, &[]).await;
    }

    let rows: Vec<(String, String, Option<Uuid>, Option<String>)> = sqlx::query_as(
        "SELECT release_key, protocol, indexer_source_id, download_url FROM release_sightings ORDER BY release_key",
    )
    .fetch_all(pool)
    .await
    .unwrap();
    let a = "a".repeat(40);
    assert_eq!(
        rows,
        vec![
            (a.clone(), "torrent".into(), Some(tv.source.id), None),
            (
                "nzb-guid".into(),
                "usenet".into(),
                Some(tv.source.id),
                Some("http://example.com/download/nzb-guid".into())
            ),
        ]
    );

    sqlx::query(
        "UPDATE release_sightings SET seen_at = NOW() - INTERVAL '15 days' WHERE release_key = $1",
    )
    .bind(&a)
    .execute(pool)
    .await
    .unwrap();
    CacheCleanupHandler::new(pool.clone())
        .execute(
            Uuid::new_v4(),
            json!({}),
            tokio_util::sync::CancellationToken::new(),
        )
        .await
        .unwrap();
    let kept: Vec<String> = sqlx::query_scalar("SELECT release_key FROM release_sightings")
        .fetch_all(pool)
        .await
        .unwrap();
    assert_eq!(kept, vec!["nzb-guid".to_string()]);
    tv.server.cleanup().await;
}
