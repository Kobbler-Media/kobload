use crate::common;

use common::TestServer;
use kobload_api::repositories::hash_index::{self, HashFileEntry, HashIndexEntry};

// Regression: the watch poller, backlog search, stream service and bitmagnet
// sync all upsert into torrent_hash_index concurrently. Rows went in in caller
// order, so two batches sharing hashes in a different order locked them in
// opposite orders and Postgres killed one with "deadlock detected" — that
// batch was simply never indexed.

const ROWS: usize = 400;
const ROUNDS: usize = 10;

fn hash(i: usize) -> String {
    format!("{i:040x}")
}

fn entry(i: usize) -> HashIndexEntry {
    HashIndexEntry {
        info_hash: hash(i),
        imdb_id: None,
        tmdb_id: None,
        original_title: format!("Some.Show.S01E{:02}.1080p.WEB", i % 100),
        parsed_title: None,
        year: None,
        quality: None,
        size: None,
        magnet_url: None,
        source: "test".to_string(),
        seeders: Some(i as i32),
        season: None,
        episodes: None,
    }
}

fn file(i: usize) -> HashFileEntry {
    HashFileEntry {
        info_hash: hash(i),
        file_idx: 0,
        filename: format!("Some.Show.S01E{:02}.mkv", i % 100),
        size: Some(1_000),
        season: Some(1),
        episodes: Some(vec![(i % 100) as i16]),
        media_type: "video".to_string(),
    }
}

#[tokio::test]
async fn concurrent_upsert_batches_in_opposite_order_do_not_deadlock() {
    let server = TestServer::start().await;
    let pool = &server.pool;

    let forward: Vec<HashIndexEntry> = (0..ROWS).map(entry).collect();
    let reverse: Vec<HashIndexEntry> = forward.iter().rev().cloned().collect();

    // Seed so every later upsert takes the ON CONFLICT DO UPDATE path and
    // holds row locks until the statement finishes.
    hash_index::upsert_batch(pool, &forward).await.unwrap();

    for round in 0..ROUNDS {
        let (a, b, c, d) = tokio::join!(
            hash_index::upsert_batch(pool, &forward),
            hash_index::upsert_batch(pool, &reverse),
            hash_index::upsert_batch(pool, &forward),
            hash_index::upsert_batch(pool, &reverse),
        );
        for (n, r) in [a, b, c, d].into_iter().enumerate() {
            assert!(r.is_ok(), "round {round} batch {n}: {:?}", r.err());
        }
    }
}

#[tokio::test]
async fn concurrent_file_upserts_in_opposite_order_do_not_deadlock() {
    let server = TestServer::start().await;
    let pool = &server.pool;

    let forward: Vec<HashFileEntry> = (0..ROWS).map(file).collect();
    let reverse: Vec<HashFileEntry> = forward.iter().rev().cloned().collect();
    let entries: Vec<HashIndexEntry> = (0..ROWS).map(entry).collect();
    let entries_rev: Vec<HashIndexEntry> = entries.iter().rev().cloned().collect();

    hash_index::upsert_files(pool, &forward).await.unwrap();

    // Files upserts also touch torrent_hash_index via ensure_parents, so mix
    // them with plain entry upserts to cover the cross-function ordering.
    for round in 0..ROUNDS {
        let (a, b, c, d) = tokio::join!(
            hash_index::upsert_files(pool, &forward),
            hash_index::upsert_files(pool, &reverse),
            hash_index::upsert_batch(pool, &entries_rev),
            hash_index::upsert_batch(pool, &entries),
        );
        assert!(a.is_ok(), "round {round} files fwd: {:?}", a.err());
        assert!(b.is_ok(), "round {round} files rev: {:?}", b.err());
        assert!(c.is_ok(), "round {round} entries rev: {:?}", c.err());
        assert!(d.is_ok(), "round {round} entries fwd: {:?}", d.err());
    }
}
