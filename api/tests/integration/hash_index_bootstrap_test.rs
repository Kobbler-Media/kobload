use crate::common;

use common::TestServer;
use sqlx::Executor;
use sqlx::postgres::PgConnection;

/// Verify that CREATE TEMP TABLE + COPY + upsert all work on a single connection.
/// This was broken when each step used the pool (different connections).
#[tokio::test]
async fn test_temp_table_copy_upsert_on_single_connection() {
    let server = TestServer::start().await;
    let pool = &server.pool;

    // Acquire a single connection — same pattern as the fixed handler
    let mut conn = pool.acquire().await.expect("acquire connection");
    let c: &mut PgConnection = &mut conn;

    // Step 1: Create temp table
    c.execute("CREATE TEMP TABLE hash_import (LIKE torrent_hash_index INCLUDING DEFAULTS)")
        .await
        .expect("create temp table");

    // Step 2: COPY a few rows into the temp table. Mirrors the handler's
    // 13-column layout (info_hash … seeders, season, episodes) — seed-processor
    // emits this format, so bootstrap must accept it end-to-end.
    let tsv_data = "abc123def456\ttt1234567\t12345\tTest.Movie.2024.1080p\tTest Movie\t2024\t{\"resolution\":\"1080p\"}\t1500000000\t\\N\ttest\t50\t\\N\t\\N\n\
                    fff999aaa111\t\\N\t67890\tAnother.Show.S01E09-12\tAnother Show\t2023\t{\"resolution\":\"720p\"}\t700000000\t\\N\ttest\t100\t1\t{9,10,11,12}\n";

    let mut copy_in = c
        .copy_in_raw(
            "COPY hash_import(info_hash, imdb_id, tmdb_id, original_title, parsed_title, year, quality, size, magnet_url, source, seeders, season, episodes) FROM STDIN WITH (FORMAT text, NULL '\\N')",
        )
        .await
        .expect("start COPY");

    copy_in.send(tsv_data.as_bytes()).await.expect("send data");

    let rows = copy_in.finish().await.expect("finish COPY");
    assert_eq!(rows, 2, "should copy 2 rows");

    // Step 3: Upsert from temp table into main table
    let result = c
        .execute(
            "INSERT INTO torrent_hash_index (
            info_hash, imdb_id, tmdb_id, original_title, parsed_title,
            year, quality, size, magnet_url, source, seeders,
            season, episodes
        )
        SELECT
            info_hash, imdb_id, tmdb_id, original_title, parsed_title,
            year, quality, size, magnet_url, source, seeders,
            season, episodes
        FROM hash_import
        ON CONFLICT (info_hash) DO UPDATE SET
            last_seen = NOW(),
            imdb_id = COALESCE(torrent_hash_index.imdb_id, EXCLUDED.imdb_id),
            tmdb_id = COALESCE(torrent_hash_index.tmdb_id, EXCLUDED.tmdb_id),
            quality = COALESCE(torrent_hash_index.quality, EXCLUDED.quality),
            size = COALESCE(torrent_hash_index.size, EXCLUDED.size),
            seeders = COALESCE(EXCLUDED.seeders, torrent_hash_index.seeders),
            season = COALESCE(torrent_hash_index.season, EXCLUDED.season),
            episodes = COALESCE(torrent_hash_index.episodes, EXCLUDED.episodes)",
        )
        .await
        .expect("upsert from temp table");

    assert_eq!(result.rows_affected(), 2);

    // Verify the episodes[] round-tripped through COPY + upsert.
    let row: (Option<i16>, Option<Vec<i16>>) = sqlx::query_as(
        "SELECT season, episodes FROM torrent_hash_index WHERE info_hash = 'fff999aaa111'",
    )
    .fetch_one(pool)
    .await
    .expect("fetch episodes");
    assert_eq!(row.0, Some(1));
    assert_eq!(row.1.as_deref(), Some(&[9i16, 10, 11, 12][..]));

    // Step 4: Clean up temp table
    c.execute("DROP TABLE IF EXISTS hash_import")
        .await
        .expect("drop temp table");
    drop(conn);

    // Verify rows are in the main table
    let row: (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM torrent_hash_index WHERE source = 'test'")
            .fetch_one(pool)
            .await
            .expect("count rows");
    assert_eq!(row.0, 2);

    server.cleanup().await;
}
