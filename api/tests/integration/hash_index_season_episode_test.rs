use crate::common;

use common::TestServer;
use kobload_api::repositories::hash_index::{
    self, HashIndexEntry, derive_season_episodes, derive_season_episodes_from_media,
};

/// `derive_season_episodes` maps parsed TV titles to (season, episodes list).
/// Multi-episode ranges now land as a list (not a single value), and packs
/// still collapse to None (= "any episode of this season").
#[test]
fn derive_season_episodes_rules() {
    assert_eq!(
        derive_season_episodes("Show.S05E01.1080p.WEB-DL.mkv"),
        (Some(5), Some(vec![1]))
    );

    let (s, e) = derive_season_episodes("Show.S05.Complete.1080p.WEB-DL");
    assert_eq!(s, Some(5), "season pack should set season");
    assert_eq!(e, None, "pack must leave episodes NULL");

    // Multi-ep range within is_pack threshold: used to collapse to None,
    // now retained as a list — this is the bug fix.
    let (s, e) = derive_season_episodes("Show.S05E01-E05.WEB-DL");
    assert_eq!(s, Some(5));
    assert_eq!(e, Some(vec![1, 2, 3, 4, 5]));

    // The canonical bug-case from the plan: S01E09-12 must yield {9,10,11,12}.
    let (s, e) = derive_season_episodes("Show.S01E09-12.WEB-DL");
    assert_eq!(s, Some(1));
    assert_eq!(e, Some(vec![9, 10, 11, 12]));

    // Movie — no S/E.
    assert_eq!(
        derive_season_episodes("The Matrix 1999 1080p BluRay x264"),
        (None, None)
    );
}

/// `derive_season_episodes_from_media` avoids the extra parse when the caller
/// already has a ParsedRelease — must agree with the title-path.
#[test]
fn derive_from_media_matches_title_path() {
    for title in [
        "Show.S03E07.720p.HDTV.x264-GROUP",
        "Show.S01E09-12.1080p.WEB-DL",
        "Show.S05.Complete.1080p",
    ] {
        let parsed = kobload_api::parsing::parse_release(title, "tv");
        assert_eq!(
            derive_season_episodes_from_media(&parsed.media),
            derive_season_episodes(title),
            "mismatch on {title}"
        );
    }
}

/// `search_by_imdb_with_files` episode-specific query (IMDB+season+episode):
/// - include single-ep torrents whose parent matches exactly,
/// - include packs with a file pinning the requested S/E,
/// - include multi-ep range torrents whose `episodes` array contains the ep,
/// - include parent-NULL rows whose file rows pin the requested S/E,
/// - exclude packs WITHOUT file data (no proof of containing the ep),
/// - exclude packs whose files don't cover the requested episode,
/// - exclude single-eps for other episodes,
/// - exclude parent-NULL rows with no file evidence (Law & Order case),
/// - exclude wrong-season parents with no matching file.
#[tokio::test]
async fn search_filters_wrong_season_episode() {
    let server = TestServer::start().await;
    let pool = &server.pool;

    let imdb = "tt9999999";

    let make = |hash: &str,
                title: &str,
                season: Option<i16>,
                episodes: Option<Vec<i16>>|
     -> HashIndexEntry {
        HashIndexEntry {
            info_hash: hash.to_string(),
            imdb_id: Some(imdb.to_string()),
            tmdb_id: None,
            original_title: title.to_string(),
            parsed_title: None,
            year: None,
            quality: None,
            size: Some(1_000_000_000),
            magnet_url: None,
            source: "test".to_string(),
            seeders: Some(10),
            season,
            episodes,
        }
    };

    let entries = vec![
        // 1. Exact single-ep match — INCLUDED.
        make(
            "1111000000000000000000000000000000000001",
            "Show.S05E01.1080p.mkv",
            Some(5),
            Some(vec![1]),
        ),
        // 2. Wrong-episode single-ep — EXCLUDED.
        make(
            "2222000000000000000000000000000000000002",
            "Show.S05E03.1080p.mkv",
            Some(5),
            Some(vec![3]),
        ),
        // 3. Wrong-season pack — EXCLUDED.
        make(
            "3333000000000000000000000000000000000003",
            "Show.S04.Complete",
            Some(4),
            None,
        ),
        // 4. S5 pack WITHOUT file data — EXCLUDED from episode-specific
        //    search (no proof it contains E1). Still INCLUDED for
        //    season-only search below.
        make(
            "4444000000000000000000000000000000000004",
            "Show.S05.Complete.NoFiles",
            Some(5),
            None,
        ),
        // 5. Damaged row (imdb set, no S/E, no files) — EXCLUDED (Law &
        //    Order Toronto case post-backfill; the escape hatch is gone).
        make(
            "5555000000000000000000000000000000000005",
            "Show.legacy.no.s.or.e.mkv",
            None,
            None,
        ),
        // 7. Multi-episode range containing the requested ep — INCLUDED.
        make(
            "7777000000000000000000000000000000000007",
            "Show.S01E09-12.WEB-DL.mkv",
            Some(1),
            Some(vec![9, 10, 11, 12]),
        ),
        // 8. Multi-episode range NOT containing the requested ep — EXCLUDED.
        make(
            "8888000000000000000000000000000000000008",
            "Show.S01E05-08.WEB-DL.mkv",
            Some(1),
            Some(vec![5, 6, 7, 8]),
        ),
    ];

    hash_index::upsert_batch(pool, &entries)
        .await
        .expect("upsert");

    // Pack WITH a matching file at (S5, E1).
    let pack_hash = "6666000000000000000000000000000000000006";
    hash_index::upsert_single(
        pool,
        &make(pack_hash, "Show.S05.Complete.WithFiles", Some(5), None),
    )
    .await
    .expect("upsert pack");

    // Parent-NULL row whose file pins S5E1 — INCLUDED via file-EXISTS branch.
    let file_pins_hash = "aaaa000000000000000000000000000000000001";
    hash_index::upsert_single(
        pool,
        &make(file_pins_hash, "Show.Episode.File.Pins.mkv", None, None),
    )
    .await
    .expect("upsert file-pins");

    sqlx::query(
        "INSERT INTO torrent_hash_files (info_hash, file_idx, filename, size, season, episodes, media_type)
         VALUES ($1, 0, 'Show.S05E01.mkv', 500000000, 5, '{1}'::smallint[], 'video'),
                ($1, 1, 'Show.S05E02.mkv', 500000000, 5, '{2}'::smallint[], 'video'),
                ($2, 0, 'Show.S05E01.parent-null.mkv', 500000000, 5, '{1}'::smallint[], 'video')",
    )
    .bind(pack_hash)
    .bind(file_pins_hash)
    .execute(pool)
    .await
    .expect("insert files");

    // ── Episode-specific: S5 / E1 ─────────────────────────────────────
    let results = hash_index::search_by_imdb_with_files(pool, imdb, Some(5), Some(1))
        .await
        .expect("search S5E1");
    let hashes: std::collections::HashSet<String> =
        results.iter().map(|r| r.info_hash.clone()).collect();

    assert!(hashes.contains("1111000000000000000000000000000000000001"));
    assert!(
        hashes.contains(pack_hash),
        "pack with a matching file must be returned (files recover the episode)"
    );
    assert!(
        hashes.contains(file_pins_hash),
        "parent-NULL row with matching file must match via file fallback"
    );

    assert!(!hashes.contains("2222000000000000000000000000000000000002"));
    assert!(!hashes.contains("3333000000000000000000000000000000000003"));
    assert!(
        !hashes.contains("4444000000000000000000000000000000000004"),
        "pack at right season with no files has no proof of containing E1 — excluded"
    );
    assert!(
        !hashes.contains("5555000000000000000000000000000000000005"),
        "damaged parent-NULL row with no file evidence must be excluded"
    );

    // ── Episode-specific: S1 / E10 ────────────────────────────────────
    let results = hash_index::search_by_imdb_with_files(pool, imdb, Some(1), Some(10))
        .await
        .expect("search S1E10");
    let hashes: std::collections::HashSet<String> =
        results.iter().map(|r| r.info_hash.clone()).collect();

    assert!(
        hashes.contains("7777000000000000000000000000000000000007"),
        "multi-ep range S01E09-12 must match S01E10 query"
    );
    assert!(
        !hashes.contains("8888000000000000000000000000000000000008"),
        "multi-ep range S01E05-08 must NOT match S01E10 query"
    );

    // ── IMDB only — returns everything for the IMDB id ───────────────
    let results = hash_index::search_by_imdb_with_files(pool, imdb, None, None)
        .await
        .expect("search IMDB-only");
    let hashes: std::collections::HashSet<String> =
        results.iter().map(|r| r.info_hash.clone()).collect();
    for h in [
        "1111000000000000000000000000000000000001",
        "2222000000000000000000000000000000000002",
        "3333000000000000000000000000000000000003",
        "4444000000000000000000000000000000000004",
        "5555000000000000000000000000000000000005",
        "7777000000000000000000000000000000000007",
        "8888000000000000000000000000000000000008",
        pack_hash,
        file_pins_hash,
    ] {
        assert!(hashes.contains(h), "IMDB-only must return {h}");
    }

    // ── Season-only (S=5, E=None) ────────────────────────────────────
    let results = hash_index::search_by_imdb_with_files(pool, imdb, Some(5), None)
        .await
        .expect("search season-only");
    let hashes: std::collections::HashSet<String> =
        results.iter().map(|r| r.info_hash.clone()).collect();

    assert!(hashes.contains("1111000000000000000000000000000000000001"));
    assert!(
        hashes.contains("4444000000000000000000000000000000000004"),
        "pack at right season with NULL episodes matches season-only search"
    );
    assert!(hashes.contains(pack_hash));
    assert!(
        hashes.contains(file_pins_hash),
        "parent-NULL with file at right season must match season-only search"
    );

    assert!(!hashes.contains("3333000000000000000000000000000000000003"));
    assert!(!hashes.contains("7777000000000000000000000000000000000007"));
    assert!(
        !hashes.contains("5555000000000000000000000000000000000005"),
        "parent-NULL with no file evidence must be excluded from season-only search"
    );

    server.cleanup().await;
}

/// Coverage for `search_by_imdb` (parent-row only path) across the three
/// query modes (IMDB-only / season-only / episode-specific).
#[tokio::test]
async fn search_by_imdb_matches_episodes_array() {
    let server = TestServer::start().await;
    let pool = &server.pool;

    let imdb = "tt4242424";

    let make = |hash: &str,
                title: &str,
                season: Option<i16>,
                episodes: Option<Vec<i16>>|
     -> HashIndexEntry {
        HashIndexEntry {
            info_hash: hash.to_string(),
            imdb_id: Some(imdb.to_string()),
            tmdb_id: None,
            original_title: title.to_string(),
            parsed_title: None,
            year: None,
            quality: None,
            size: Some(1_000_000_000),
            magnet_url: None,
            source: "test".to_string(),
            seeders: Some(10),
            season,
            episodes,
        }
    };

    // Multi-ep range S01E09-12.
    hash_index::upsert_single(
        pool,
        &make(
            "abcd000000000000000000000000000000000001",
            "Show.S01E09-12.WEB-DL.mkv",
            Some(1),
            Some(vec![9, 10, 11, 12]),
        ),
    )
    .await
    .expect("upsert multi-ep");

    // Season pack S01 (episodes NULL — pack).
    hash_index::upsert_single(
        pool,
        &make(
            "abcd000000000000000000000000000000000002",
            "Show.S01.Complete",
            Some(1),
            None,
        ),
    )
    .await
    .expect("upsert pack");

    // Parent-NULL damaged row (no file fallback in this variant → excluded).
    hash_index::upsert_single(
        pool,
        &make(
            "abcd000000000000000000000000000000000003",
            "Show.damaged.no.s.or.e",
            None,
            None,
        ),
    )
    .await
    .expect("upsert damaged");

    // Wrong-season row.
    hash_index::upsert_single(
        pool,
        &make(
            "abcd000000000000000000000000000000000004",
            "Show.S02E01",
            Some(2),
            Some(vec![1]),
        ),
    )
    .await
    .expect("upsert wrong-season");

    // ── Episode-specific: S1/E10 ──────────────────────────────────────
    let results = hash_index::search_by_imdb(pool, imdb, Some(1), Some(10))
        .await
        .expect("search S1E10");
    let hashes: std::collections::HashSet<String> =
        results.iter().map(|r| r.info_hash.clone()).collect();
    assert!(hashes.contains("abcd000000000000000000000000000000000001"));
    assert!(
        !hashes.contains("abcd000000000000000000000000000000000002"),
        "S1 pack (episodes NULL) must NOT match episode-specific query via \
         the parent-only variant — no proof of containing E10"
    );
    assert!(
        !hashes.contains("abcd000000000000000000000000000000000003"),
        "parent-NULL damaged row must NOT match"
    );
    assert!(!hashes.contains("abcd000000000000000000000000000000000004"));

    // ── Episode-specific: S1/E3 — pack doesn't match either. ─────────
    let results = hash_index::search_by_imdb(pool, imdb, Some(1), Some(3))
        .await
        .expect("search S1E3");
    let hashes: std::collections::HashSet<String> =
        results.iter().map(|r| r.info_hash.clone()).collect();
    assert!(
        !hashes.contains("abcd000000000000000000000000000000000001"),
        "S01E09-12 must NOT match S1E3"
    );
    assert!(
        !hashes.contains("abcd000000000000000000000000000000000002"),
        "pack must NOT match episode-specific query in the parent-only variant"
    );

    // ── Season-only (S=1, E=None) — packs pass here. ─────────────────
    let results = hash_index::search_by_imdb(pool, imdb, Some(1), None)
        .await
        .expect("search season-only");
    let hashes: std::collections::HashSet<String> =
        results.iter().map(|r| r.info_hash.clone()).collect();
    assert!(hashes.contains("abcd000000000000000000000000000000000001"));
    assert!(
        hashes.contains("abcd000000000000000000000000000000000002"),
        "S1 pack must match season-only search"
    );
    assert!(!hashes.contains("abcd000000000000000000000000000000000003"));
    assert!(!hashes.contains("abcd000000000000000000000000000000000004"));

    // ── IMDB only ────────────────────────────────────────────────────
    let results = hash_index::search_by_imdb(pool, imdb, None, None)
        .await
        .expect("search IMDB-only");
    let hashes: std::collections::HashSet<String> =
        results.iter().map(|r| r.info_hash.clone()).collect();
    for h in [
        "abcd000000000000000000000000000000000001",
        "abcd000000000000000000000000000000000002",
        "abcd000000000000000000000000000000000003",
        "abcd000000000000000000000000000000000004",
    ] {
        assert!(hashes.contains(h), "IMDB-only must return {h}");
    }

    server.cleanup().await;
}

/// Upserting the same hash with parsed S/episodes fields updates them via COALESCE —
/// existing non-null values win, so a re-ingest can't clobber good data with NULLs.
#[tokio::test]
async fn upsert_coalesces_season_episodes() {
    let server = TestServer::start().await;
    let pool = &server.pool;

    let hash = "cafecafe00000000000000000000000000000001";

    let make = |season: Option<i16>, episodes: Option<Vec<i16>>| HashIndexEntry {
        info_hash: hash.to_string(),
        imdb_id: Some("tt7777777".to_string()),
        tmdb_id: None,
        original_title: "Show.S02E04.WEB-DL".to_string(),
        parsed_title: None,
        year: None,
        quality: None,
        size: None,
        magnet_url: None,
        source: "test".to_string(),
        seeders: None,
        season,
        episodes,
    };

    // First insert: both set.
    hash_index::upsert_single(pool, &make(Some(2), Some(vec![4])))
        .await
        .expect("first upsert");

    // Second upsert: NULL fields (simulating an ingest that failed to parse).
    hash_index::upsert_single(pool, &make(None, None))
        .await
        .expect("second upsert");

    // Existing (2, [4]) must survive.
    let row: (Option<i16>, Option<Vec<i16>>) =
        sqlx::query_as("SELECT season, episodes FROM torrent_hash_index WHERE info_hash = $1")
            .bind(hash)
            .fetch_one(pool)
            .await
            .expect("fetch");

    assert_eq!(row.0, Some(2));
    assert_eq!(row.1.as_deref(), Some(&[4][..]));

    server.cleanup().await;
}
