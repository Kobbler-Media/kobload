use crate::common;

use common::TestServer;
use kobload_api::repositories::hash_index;

/// Regression test: external API results (Torrentio, StremThru, EZTV) must be
/// persisted to torrent_hash_index via upsert_batch. Previously, results were
/// converted to Release structs (with indexer "hash_index:{source}") before
/// indexing, and background_index_releases filtered them all out because they
/// started with "hash_index:".
///
/// The fix returns raw HashIndexEntry from search_external_apis and persists
/// them directly — bypassing the Release conversion entirely.
#[tokio::test]
async fn test_external_api_entries_persist_to_hash_index() {
    let server = TestServer::start().await;
    let pool = &server.pool;

    // Simulate entries that would come from Torrentio, StremThru, and EZTV
    let entries = vec![
        hash_index::HashIndexEntry {
            info_hash: "aaaa000000000000000000000000000000000001".to_string(),
            imdb_id: Some("tt0098844".to_string()),
            tmdb_id: None,
            original_title: "Law & Order (1990) - S08E17 - Carrier (1080p AMZN WEB-DL x265 ImE)"
                .to_string(),
            parsed_title: Some("Law & Order".to_string()),
            year: Some(1990),
            quality: serde_json::to_value(serde_json::json!({"resolution": "1080p"})).ok(),
            size: Some(500_000_000),
            magnet_url: None,
            source: "torrentio".to_string(),
            seeders: Some(42),
            season: Some(8),
            episodes: Some(vec![17]),
        },
        hash_index::HashIndexEntry {
            info_hash: "bbbb000000000000000000000000000000000002".to_string(),
            imdb_id: Some("tt0098844".to_string()),
            tmdb_id: None,
            original_title: "Law.And.Order.S08E17.hdtv".to_string(),
            parsed_title: Some("Law And Order".to_string()),
            year: None,
            quality: None,
            size: Some(350_000_000),
            magnet_url: None,
            source: "stremthru".to_string(),
            seeders: None,
            season: Some(8),
            episodes: Some(vec![17]),
        },
        hash_index::HashIndexEntry {
            info_hash: "cccc000000000000000000000000000000000003".to_string(),
            imdb_id: Some("tt0098844".to_string()),
            tmdb_id: None,
            original_title: "Law.and.Order.S08E17.720p.HDTV".to_string(),
            parsed_title: Some("Law and Order".to_string()),
            year: None,
            quality: serde_json::to_value(serde_json::json!({"resolution": "720p"})).ok(),
            size: Some(700_000_000),
            magnet_url: None,
            source: "eztv".to_string(),
            seeders: Some(15),
            season: Some(8),
            episodes: Some(vec![17]),
        },
    ];

    // This is the call the fix makes — upsert_batch with raw entries
    let rows_affected = hash_index::upsert_batch(pool, &entries)
        .await
        .expect("upsert_batch should succeed") as u64;

    assert_eq!(rows_affected, 3, "all 3 entries should be inserted");

    // Verify each source is persisted
    for source in ["torrentio", "stremthru", "eztv"] {
        let count: (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM torrent_hash_index WHERE source = $1 AND imdb_id = 'tt0098844'",
        )
        .bind(source)
        .fetch_one(pool)
        .await
        .expect("count query");

        assert!(
            count.0 > 0,
            "expected at least 1 entry with source={source}"
        );
    }

    // Verify search_by_imdb finds them (no season/episode filter — returns all)
    let results: Vec<hash_index::HashIndexRow> =
        hash_index::search_by_imdb(pool, "tt0098844", None, None)
            .await
            .expect("search_by_imdb");

    assert_eq!(results.len(), 3, "all 3 entries should be searchable");

    server.cleanup().await;
}
