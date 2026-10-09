// Offline one-time tool — clippy pedantry suppressed for pragmatism
#![allow(clippy::all)]
//! Pre-processor for torrent hash index seed data.
//!
//! Reads bulk dump sources (RARBG, MagnetDB, TPB, RuTracker, DMM), filters to
//! video/TV/books/sports content, enriches with IMDB IDs from multiple sources,
//! deduplicates by info_hash, and writes a PostgreSQL COPY-ready CSV.
//!
//! Usage:
//!   cargo run --bin seed-processor -- --seeds-dir data/seeds --output data/seeds/processed/hash_index.csv

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use kobload_api::parsing::{ParsedMedia, parse_release};

/// A processed hash index entry ready for CSV output.
struct ProcessedEntry {
    info_hash: String,
    imdb_id: Option<String>,
    tmdb_id: Option<String>,
    original_title: String,
    parsed_title: String,
    year: Option<i16>,
    quality_json: Option<String>,
    size: Option<i64>,
    source: String,
    season: Option<i16>,
    episodes: Option<Vec<i16>>,
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let seeds_dir = args
        .iter()
        .position(|a| a == "--seeds-dir")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("data/seeds"));

    let output_path = args
        .iter()
        .position(|a| a == "--output")
        .and_then(|i| args.get(i + 1))
        .map(PathBuf::from)
        .unwrap_or_else(|| seeds_dir.join("processed/hash_index.csv"));

    eprintln!("=== Hash Index Seed Processor ===");
    eprintln!("Seeds dir: {}", seeds_dir.display());
    eprintln!("Output:    {}", output_path.display());
    eprintln!();

    let start = Instant::now();

    // Step 1: Load IMDB title dump for offline enrichment
    let imdb_map = load_imdb_titles(&seeds_dir.join("imdb/title.basics.tsv.gz"));

    // Step 2: Load MagnetDB matchedVideos for hash→IMDB enrichment
    // This gives us torrent_id→IMDB, but we need info_hash→torrent_id from the main dump
    let matched_videos = load_matched_videos(&seeds_dir.join("magnetdb/matchedVideos.csv"));

    // Step 3: Load MagnetDB torrent_id→info_hash mapping (from the decompressed SQLite)
    let magnetdb_hash_map = load_magnetdb_hash_map(&seeds_dir.join("magnetdb"));

    // Build combined hash→IMDB from matchedVideos + magnetdb
    let mut matched_hash_to_imdb: HashMap<String, String> = HashMap::new();
    for (torrent_id, imdb_id) in &matched_videos {
        if let Some(info_hash) = magnetdb_hash_map.get(torrent_id) {
            matched_hash_to_imdb.insert(info_hash.clone(), imdb_id.clone());
        }
    }
    eprintln!(
        "MagnetDB enrichment: {} hashes with IMDB IDs",
        matched_hash_to_imdb.len()
    );

    // Step 4: Process all sources into a deduplicated map
    let mut entries: HashMap<String, ProcessedEntry> = HashMap::new();

    // RARBG first (best IMDB coverage)
    process_rarbg(&seeds_dir, &imdb_map, &matched_hash_to_imdb, &mut entries);

    // MagnetDB main dump (huge, enriched via matchedVideos)
    process_magnetdb(&seeds_dir, &imdb_map, &matched_hash_to_imdb, &mut entries);

    // TPB
    process_tpb(&seeds_dir, &imdb_map, &matched_hash_to_imdb, &mut entries);

    // RuTracker
    process_rutracker(&seeds_dir, &imdb_map, &matched_hash_to_imdb, &mut entries);

    // DMM hashlists
    process_dmm(&seeds_dir, &imdb_map, &matched_hash_to_imdb, &mut entries);

    eprintln!();
    eprintln!("=== TOTALS ===");
    eprintln!("Unique hashes: {}", entries.len());
    let with_imdb = entries.values().filter(|e| e.imdb_id.is_some()).count();
    eprintln!(
        "With IMDB ID:  {} ({:.1}%)",
        with_imdb,
        with_imdb as f64 / entries.len() as f64 * 100.0
    );

    // Step 5: Write CSV
    eprintln!();
    eprintln!("Writing CSV to {}", output_path.display());
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent).expect("create output dir");
    }
    write_csv(&output_path, &entries);

    let elapsed = start.elapsed();
    eprintln!("Done in {:.1}s", elapsed.as_secs_f64());
}

// ---------------------------------------------------------------------------
// IMDB title dump
// ---------------------------------------------------------------------------

/// Load IMDB titles into a map of (lowercase_title, year) → imdb_id.
fn load_imdb_titles(path: &Path) -> HashMap<(String, i16), String> {
    if !path.exists() {
        eprintln!(
            "IMDB title dump not found at {} — skipping IMDB enrichment",
            path.display()
        );
        return HashMap::new();
    }

    eprintln!("Loading IMDB title dump from {}...", path.display());
    let start = Instant::now();

    let file = std::fs::File::open(path).expect("open IMDB dump");
    let decoder = flate2::read::GzDecoder::new(file);
    let reader = BufReader::with_capacity(1024 * 1024, decoder);

    let mut map: HashMap<(String, i16), String> = HashMap::new();
    let mut count = 0u64;

    for line in reader.lines().skip(1) {
        // header is skipped
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };

        // TSV: tconst, titleType, primaryTitle, originalTitle, isAdult, startYear, endYear, runtimeMinutes, genres
        let fields: Vec<&str> = line.split('\t').collect();
        if fields.len() < 9 {
            continue;
        }

        let tconst = fields[0]; // tt1234567
        let title_type = fields[1];
        let primary_title = fields[2];
        let is_adult = fields[4];
        let start_year = fields[5];

        // Filter: only video content types, skip adult
        if is_adult == "1" {
            continue;
        }
        if !matches!(
            title_type,
            "movie"
                | "tvSeries"
                | "tvMiniSeries"
                | "tvMovie"
                | "tvSpecial"
                | "short"
                | "video"
                | "tvShort"
                | "tvEpisode"
        ) {
            continue;
        }

        if let Ok(year) = start_year.parse::<i16>() {
            let key = (primary_title.to_lowercase(), year);
            // First entry wins (keeps the more canonical tconst)
            map.entry(key).or_insert_with(|| tconst.to_string());
        }

        count += 1;
    }

    eprintln!(
        "IMDB: loaded {} titles ({} unique keys) in {:.1}s",
        count,
        map.len(),
        start.elapsed().as_secs_f64()
    );
    map
}

// ---------------------------------------------------------------------------
// MagnetDB matchedVideos
// ---------------------------------------------------------------------------

/// Load matchedVideos CSV: torrent_id → IMDB ID.
fn load_matched_videos(path: &Path) -> HashMap<i64, String> {
    if !path.exists() {
        eprintln!("matchedVideos not found at {} — skipping", path.display());
        return HashMap::new();
    }

    eprintln!("Loading matchedVideos from {}...", path.display());
    let start = Instant::now();

    let mut reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .from_path(path)
        .expect("open matchedVideos");

    let headers = reader.headers().expect("read headers").clone();
    let torrent_id_idx = headers.iter().position(|h| h == "torrent_id");
    let imdb_idx = headers.iter().position(|h| h == "movieMatchTitleId");

    let (Some(tid_idx), Some(iid_idx)) = (torrent_id_idx, imdb_idx) else {
        eprintln!("matchedVideos: missing torrent_id or movieMatchTitleId columns");
        return HashMap::new();
    };

    let mut map: HashMap<i64, String> = HashMap::new();

    for result in reader.records() {
        let record = match result {
            Ok(r) => r,
            Err(_) => continue,
        };

        let torrent_id: i64 = match record.get(tid_idx).and_then(|v| v.parse().ok()) {
            Some(id) => id,
            None => continue,
        };

        let imdb_id = match record.get(iid_idx) {
            Some(id) if id.starts_with("tt") && id.len() > 2 => id.to_string(),
            _ => continue,
        };

        map.insert(torrent_id, imdb_id);
    }

    eprintln!(
        "matchedVideos: {} torrent_id→IMDB mappings in {:.1}s",
        map.len(),
        start.elapsed().as_secs_f64()
    );
    map
}

/// Load MagnetDB SQLite torrent_id→info_hash_hex mapping.
fn load_magnetdb_hash_map(magnetdb_dir: &Path) -> HashMap<i64, String> {
    // Look for decompressed SQLite
    let candidates = [
        magnetdb_dir.join("magnetdb_public.sqlite3"),
        magnetdb_dir.join("MagnetDB_2024/magnetdb_public.sqlite3"),
    ];

    let db_path = match candidates.iter().find(|p| p.exists()) {
        Some(p) => p,
        None => {
            eprintln!("MagnetDB SQLite not found (not yet decompressed?) — skipping hash map");
            return HashMap::new();
        }
    };

    eprintln!("Loading MagnetDB hash map from {}...", db_path.display());
    let start = Instant::now();

    let conn =
        rusqlite::Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .expect("open MagnetDB SQLite");

    // Find the torrents table
    let table = find_table_with_column(&conn, "info_hash_hex")
        .or_else(|| find_table_with_column(&conn, "info_hash"))
        .expect("MagnetDB must have a table with info_hash column");

    let hash_col = if has_column(&conn, &table, "info_hash_hex") {
        "info_hash_hex"
    } else {
        "info_hash"
    };

    // info_hash may be BLOB or TEXT — detect and use hex() SQL function for BLOBs
    let is_blob = conn
        .prepare(&format!("PRAGMA table_info(\"{table}\")"))
        .ok()
        .and_then(|mut stmt| {
            stmt.query_map([], |row| {
                Ok((row.get::<_, String>(1)?, row.get::<_, String>(2)?))
            })
            .ok()
            .map(|rows| {
                rows.filter_map(|r| r.ok())
                    .any(|(name, typ)| name == hash_col && typ.to_uppercase() == "BLOB")
            })
        })
        .unwrap_or(false);

    eprintln!(
        "  MagnetDB hash column '{hash_col}' is {}",
        if is_blob { "BLOB" } else { "TEXT" }
    );

    let hash_expr = if is_blob {
        format!("lower(hex(\"{hash_col}\"))")
    } else {
        format!("lower(\"{hash_col}\")")
    };

    let query = format!("SELECT id, {hash_expr} FROM \"{table}\"");
    let mut stmt = conn.prepare(&query).expect("prepare MagnetDB query");

    let mut map: HashMap<i64, String> = HashMap::new();
    let mut count = 0u64;

    let rows = stmt
        .query_map([], |row| {
            let id: i64 = row.get(0)?;
            let hash: String = row.get(1)?;
            Ok((id, hash))
        })
        .expect("query MagnetDB");

    for row in rows {
        if let Ok((id, hash)) = row {
            if hash.len() == 40 {
                map.insert(id, hash);
            }
            count += 1;
            if count % 5_000_000 == 0 {
                eprintln!("  MagnetDB hash map: {}M rows...", count / 1_000_000);
            }
        }
    }

    eprintln!(
        "MagnetDB hash map: {} entries (from {} rows) in {:.1}s",
        map.len(),
        count,
        start.elapsed().as_secs_f64()
    );
    map
}

fn find_table_with_column(conn: &rusqlite::Connection, column: &str) -> Option<String> {
    let mut stmt = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table'")
        .ok()?;
    let tables: Vec<String> = stmt
        .query_map([], |row| row.get(0))
        .ok()?
        .filter_map(|r| r.ok())
        .collect();

    for table in tables {
        if has_column(conn, &table, column) {
            return Some(table);
        }
    }
    None
}

fn has_column(conn: &rusqlite::Connection, table: &str, column: &str) -> bool {
    conn.prepare(&format!("PRAGMA table_info(\"{table}\")"))
        .ok()
        .and_then(|mut stmt| {
            stmt.query_map([], |row| row.get::<_, String>(1))
                .ok()
                .map(|rows| rows.filter_map(|r| r.ok()).any(|c| c == column))
        })
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// Content filtering
// ---------------------------------------------------------------------------

/// Returns true if the release title looks like video/TV/books/sports content.
/// Rejects: games, software, music, XXX, and unidentifiable content.
fn is_wanted_content(title: &str) -> bool {
    let lower = title.to_lowercase();

    // XXX rejection (before anything else)
    if lower.contains("xxx")
        || lower.contains("porn")
        || lower.contains("brazzers")
        || lower.contains("naughtyamerica")
        || lower.contains("bangbros")
        || lower.contains("realitykings")
        || lower.contains("onlyfans")
    {
        return false;
    }

    // Software/games rejection
    if lower.contains(".exe")
        || lower.contains(".msi")
        || lower.contains(".dmg")
        || lower.contains(".apk")
        || lower.contains("-razor1911")
        || lower.contains("-skidrow")
        || lower.contains("-codex")
        || lower.contains("-fitgirl")
        || lower.contains("-plaza")
        || lower.contains("-reloaded")
        || lower.contains("-empress")
        || lower.contains("macos")
        || lower.contains("portable")
        || lower.contains("crack")
        || lower.contains("keygen")
        || lower.contains("patch") && lower.contains("game")
    {
        return false;
    }

    // Music rejection
    if (lower.contains("flac") || lower.contains(".mp3") || lower.contains("discography"))
        && !lower.contains("1080p")
        && !lower.contains("720p")
        && !lower.contains("2160p")
        && !lower.contains("web-dl")
        && !lower.contains("bluray")
    {
        return false;
    }

    // Positive signals for wanted content
    let has_video_markers = lower.contains("1080p")
        || lower.contains("720p")
        || lower.contains("2160p")
        || lower.contains("480p")
        || lower.contains("x264")
        || lower.contains("x265")
        || lower.contains("h264")
        || lower.contains("h.264")
        || lower.contains("hevc")
        || lower.contains("h265")
        || lower.contains("web-dl")
        || lower.contains("webrip")
        || lower.contains("bluray")
        || lower.contains("bdrip")
        || lower.contains("hdtv")
        || lower.contains("dvdrip")
        || lower.contains("remux")
        || lower.contains("brrip")
        || lower.contains("hdrip")
        || lower.contains("av1")
        || lower.contains("xvid")
        || lower.contains("divx");

    let has_tv_markers = lower.contains(".s0")
        || lower.contains(".s1")
        || lower.contains(".s2")
        || lower.contains(".s3")
        || lower.contains("s01")
        || lower.contains("s02")
        || lower.contains("season")
        || lower.contains("episode");

    let has_book_markers = lower.contains(".epub")
        || lower.contains(".mobi")
        || lower.contains(".azw")
        || lower.contains("audiobook")
        || lower.contains("unabridged")
        || lower.contains(".m4b");

    let has_sports_markers = lower.contains("formula")
        || lower.contains("f1")
        || lower.contains("motogp")
        || lower.contains("nfl")
        || lower.contains("nba")
        || lower.contains("ufc")
        || lower.contains("premier league")
        || lower.contains("champions league")
        || lower.contains("world cup")
        || lower.contains("olympics")
        || lower.contains("boxing")
        || lower.contains("wwe");

    has_video_markers || has_tv_markers || has_book_markers || has_sports_markers
}

// ---------------------------------------------------------------------------
// Enrichment helpers
// ---------------------------------------------------------------------------

/// Try to find an IMDB ID for a title using parsed metadata + IMDB dump.
fn lookup_imdb(
    title: &str,
    year: Option<i16>,
    imdb_map: &HashMap<(String, i16), String>,
    hash_imdb: &HashMap<String, String>,
    info_hash: &str,
) -> Option<String> {
    // First check matchedVideos enrichment (most reliable)
    if let Some(imdb) = hash_imdb.get(info_hash) {
        return Some(imdb.clone());
    }

    // Then try IMDB title dump (title+year exact match)
    if let Some(y) = year {
        let key = (title.to_lowercase(), y);
        if let Some(imdb) = imdb_map.get(&key) {
            return Some(imdb.clone());
        }
    }

    None
}

/// Derive (season, episodes) from a ParsedMedia matching the rules in
/// `api/src/repositories/hash_index.rs::derive_season_episodes_from_media`.
fn season_episodes_from_media(media: &ParsedMedia) -> (Option<i16>, Option<Vec<i16>>) {
    let ParsedMedia::Tv(tv) = media else {
        return (None, None);
    };
    let season = tv.season.and_then(|s| i16::try_from(s).ok());
    let episodes = if tv.is_pack || tv.episodes.is_empty() {
        None
    } else {
        let list: Vec<i16> = tv
            .episodes
            .iter()
            .filter_map(|e| i16::try_from(*e).ok())
            .collect();
        if list.is_empty() { None } else { Some(list) }
    };
    (season, episodes)
}

fn parse_and_enrich(
    raw_title: &str,
    media_kind: &str,
    info_hash: &str,
    existing_imdb: Option<&str>,
    imdb_map: &HashMap<(String, i16), String>,
    hash_imdb: &HashMap<String, String>,
) -> Option<(
    String,
    Option<i16>,
    Option<String>,
    Option<String>,
    Option<i16>,
    Option<Vec<i16>>,
)> {
    let parsed = parse_release(raw_title, media_kind);
    let quality_json = serde_json::to_value(&parsed.quality)
        .ok()
        .and_then(|v| serde_json::to_string(&v).ok());

    let year = match &parsed.media {
        ParsedMedia::Movie(m) => m.year.and_then(|y| i16::try_from(y).ok()),
        _ => None,
    };

    let imdb_id = existing_imdb
        .map(String::from)
        .or_else(|| lookup_imdb(&parsed.title, year, imdb_map, hash_imdb, info_hash));

    // S/E extraction: first try the primary parse (respects caller's media_kind),
    // then fall back to a fresh "tv" parse so TV content hidden under media_kind="movies"
    // still gets its season/episodes. Mirrors kobload's hash_index::derive_season_episodes.
    let (mut season, mut episodes) = season_episodes_from_media(&parsed.media);
    if season.is_none() {
        let tv_parsed = parse_release(raw_title, "tv");
        let (tv_season, tv_episodes) = season_episodes_from_media(&tv_parsed.media);
        season = tv_season;
        episodes = tv_episodes;
    }

    Some((parsed.title, year, quality_json, imdb_id, season, episodes))
}

#[allow(clippy::too_many_arguments)]
fn insert_entry(
    entries: &mut HashMap<String, ProcessedEntry>,
    info_hash: String,
    original_title: String,
    parsed_title: String,
    year: Option<i16>,
    quality_json: Option<String>,
    size: Option<i64>,
    source: &str,
    imdb_id: Option<String>,
    season: Option<i16>,
    episodes: Option<Vec<i16>>,
) {
    entries
        .entry(info_hash.clone())
        .and_modify(|existing| {
            // Enrich: fill in nulls from new data
            if existing.imdb_id.is_none() && imdb_id.is_some() {
                existing.imdb_id.clone_from(&imdb_id);
            }
            if existing.size.is_none() && size.is_some() {
                existing.size = size;
            }
            if existing.quality_json.is_none() && quality_json.is_some() {
                existing.quality_json.clone_from(&quality_json);
            }
            if existing.season.is_none() && season.is_some() {
                existing.season = season;
            }
            if existing.episodes.is_none() && episodes.is_some() {
                existing.episodes.clone_from(&episodes);
            }
        })
        .or_insert_with(|| ProcessedEntry {
            info_hash,
            imdb_id,
            tmdb_id: None,
            original_title,
            parsed_title,
            year,
            quality_json,
            size,
            source: source.to_string(),
            season,
            episodes,
        });
}

// ---------------------------------------------------------------------------
// Source processors
// ---------------------------------------------------------------------------

fn process_rarbg(
    seeds_dir: &Path,
    imdb_map: &HashMap<(String, i16), String>,
    hash_imdb: &HashMap<String, String>,
    entries: &mut HashMap<String, ProcessedEntry>,
) {
    let db_path = seeds_dir.join("rarbg/rarbg_db.sqlite");
    if !db_path.exists() {
        eprintln!("RARBG: not found, skipping");
        return;
    }

    eprintln!("Processing RARBG...");
    let start = Instant::now();

    let conn =
        rusqlite::Connection::open_with_flags(&db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .expect("open RARBG");

    let mut stmt = conn
        .prepare("SELECT hash, title, cat, imdb, size FROM items")
        .expect("prepare RARBG query");

    let mut total = 0u64;
    let mut kept = 0u64;

    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<i64>>(4)?,
            ))
        })
        .expect("query RARBG");

    for row in rows {
        let (hash, title, cat, imdb, size) = match row {
            Ok(r) => r,
            Err(_) => continue,
        };
        total += 1;

        // Filter by category: keep movies, TV, ebooks
        let cat_str = cat.as_deref().unwrap_or("");
        if cat_str.contains("xxx") {
            continue;
        }
        let is_video_cat = cat_str.contains("movie") || cat_str.contains("tv");
        let is_ebook_cat = cat_str.contains("ebook") || cat_str.contains("book");

        if !is_video_cat && !is_ebook_cat && !is_wanted_content(&title) {
            continue;
        }

        let media_kind = if cat_str.contains("tv") {
            "tv"
        } else if cat_str.contains("ebook") || cat_str.contains("book") {
            "ebooks"
        } else {
            "movies"
        };

        let info_hash = hash.to_lowercase();
        let existing_imdb = imdb
            .as_ref()
            .filter(|id| id.starts_with("tt") && id.len() > 2)
            .map(|s| s.as_str());

        if let Some((parsed_title, year, quality_json, imdb_id, season, episodes)) =
            parse_and_enrich(
                &title,
                media_kind,
                &info_hash,
                existing_imdb,
                imdb_map,
                hash_imdb,
            )
        {
            insert_entry(
                entries,
                info_hash,
                title,
                parsed_title,
                year,
                quality_json,
                size,
                "rarbg_dump",
                imdb_id,
                season,
                episodes,
            );
            kept += 1;
        }

        if total % 500_000 == 0 {
            eprintln!("  RARBG: {total} processed, {kept} kept...");
        }
    }

    eprintln!(
        "RARBG: {kept}/{total} kept in {:.1}s (total unique: {})",
        start.elapsed().as_secs_f64(),
        entries.len()
    );
}

fn process_magnetdb(
    seeds_dir: &Path,
    imdb_map: &HashMap<(String, i16), String>,
    hash_imdb: &HashMap<String, String>,
    entries: &mut HashMap<String, ProcessedEntry>,
) {
    let magnetdb_dir = seeds_dir.join("magnetdb");

    // Find the decompressed SQLite
    let candidates = [
        magnetdb_dir.join("magnetdb_public.sqlite3"),
        magnetdb_dir.join("MagnetDB_2024/magnetdb_public.sqlite3"),
    ];

    let db_path = match candidates.iter().find(|p| p.exists()) {
        Some(p) => p,
        None => {
            eprintln!("MagnetDB: SQLite not found (not decompressed yet?), skipping");
            return;
        }
    };

    eprintln!("Processing MagnetDB (this is the big one)...");
    let start = Instant::now();

    let conn =
        rusqlite::Connection::open_with_flags(db_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .expect("open MagnetDB");

    // Find the right table and columns
    let table = find_table_with_column(&conn, "info_hash_hex")
        .or_else(|| find_table_with_column(&conn, "info_hash"))
        .expect("MagnetDB needs info_hash column");

    let hash_col = if has_column(&conn, &table, "info_hash_hex") {
        "info_hash_hex"
    } else {
        "info_hash"
    };
    let name_col = if has_column(&conn, &table, "name") {
        "name"
    } else {
        "title"
    };
    let size_col = if has_column(&conn, &table, "total_size") {
        Some("total_size")
    } else if has_column(&conn, &table, "size") {
        Some("size")
    } else {
        None
    };

    // Check if info_hash is BLOB type — use hex() SQL function to convert
    let is_blob = conn
        .prepare(&format!("PRAGMA table_info(\"{table}\")"))
        .ok()
        .and_then(|mut stmt| {
            stmt.query_map([], |row| {
                Ok((row.get::<_, String>(1)?, row.get::<_, String>(2)?))
            })
            .ok()
            .map(|rows| {
                rows.filter_map(|r| r.ok()).any(|(col_name, col_type)| {
                    col_name == hash_col && col_type.to_uppercase() == "BLOB"
                })
            })
        })
        .unwrap_or(false);

    let hash_expr = if is_blob {
        format!("lower(hex(\"{hash_col}\"))")
    } else {
        format!("lower(\"{hash_col}\")")
    };

    let query = if let Some(sc) = size_col {
        format!("SELECT {hash_expr}, \"{name_col}\", \"{sc}\" FROM \"{table}\"")
    } else {
        format!("SELECT {hash_expr}, \"{name_col}\", NULL FROM \"{table}\"")
    };

    eprintln!("  MagnetDB query: {query}");

    let mut stmt = conn.prepare(&query).expect("prepare MagnetDB query");
    let mut total = 0u64;
    let mut kept = 0u64;

    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<i64>>(2)?,
            ))
        })
        .expect("query MagnetDB");

    for row in rows {
        let (info_hash, name, size) = match row {
            Ok(r) => r,
            Err(_) => continue,
        };
        total += 1;

        if info_hash.len() != 40 {
            continue;
        }

        // Enrich existing entries with IMDB from matchedVideos even if we skip
        if entries.contains_key(&info_hash) {
            if let Some(imdb) = hash_imdb.get(&info_hash) {
                if let Some(existing) = entries.get_mut(&info_hash) {
                    if existing.imdb_id.is_none() {
                        existing.imdb_id = Some(imdb.clone());
                    }
                }
            }
            continue;
        }

        if !is_wanted_content(&name) {
            continue;
        }

        let media_kind = guess_media_kind(&name);

        if let Some((parsed_title, year, quality_json, imdb_id, season, episodes)) =
            parse_and_enrich(&name, media_kind, &info_hash, None, imdb_map, hash_imdb)
        {
            insert_entry(
                entries,
                info_hash,
                name,
                parsed_title,
                year,
                quality_json,
                size,
                "magnetdb",
                imdb_id,
                season,
                episodes,
            );
            kept += 1;
        }

        if total % 5_000_000 == 0 {
            eprintln!(
                "  MagnetDB: {}M processed, {kept} kept...",
                total / 1_000_000
            );
        }
    }

    eprintln!(
        "MagnetDB: {kept}/{total} kept in {:.1}s (total unique: {})",
        start.elapsed().as_secs_f64(),
        entries.len()
    );
}

fn process_tpb(
    seeds_dir: &Path,
    imdb_map: &HashMap<(String, i16), String>,
    hash_imdb: &HashMap<String, String>,
    entries: &mut HashMap<String, ProcessedEntry>,
) {
    let piratebay_dir = seeds_dir.join("piratebay");
    if !piratebay_dir.exists() {
        eprintln!("TPB: directory not found, skipping");
        return;
    }

    let csv_files: Vec<PathBuf> = std::fs::read_dir(&piratebay_dir)
        .expect("read piratebay dir")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("piratebay") && n.ends_with(".csv"))
        })
        .collect();

    if csv_files.is_empty() {
        eprintln!("TPB: no CSV files found, skipping");
        return;
    }

    eprintln!("Processing TPB ({} files)...", csv_files.len());
    let start = Instant::now();
    let mut total = 0u64;
    let mut kept = 0u64;

    for csv_path in &csv_files {
        process_semicolon_csv(
            csv_path, "tpb", imdb_map, hash_imdb, entries, &mut total, &mut kept,
        );
    }

    eprintln!(
        "TPB: {kept}/{total} kept in {:.1}s (total unique: {})",
        start.elapsed().as_secs_f64(),
        entries.len()
    );
}

fn process_rutracker(
    seeds_dir: &Path,
    imdb_map: &HashMap<(String, i16), String>,
    hash_imdb: &HashMap<String, String>,
    entries: &mut HashMap<String, ProcessedEntry>,
) {
    let rt_dir = seeds_dir.join("rutracker/rutracker_2020_09_27");
    if !rt_dir.exists() {
        eprintln!("RuTracker: not found, skipping");
        return;
    }

    let csv_files: Vec<PathBuf> = std::fs::read_dir(&rt_dir)
        .expect("read rutracker dir")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("csv"))
        .collect();

    eprintln!("Processing RuTracker ({} files)...", csv_files.len());
    let start = Instant::now();
    let mut total = 0u64;
    let mut kept = 0u64;

    for csv_path in &csv_files {
        process_semicolon_csv(
            csv_path,
            "rutracker",
            imdb_map,
            hash_imdb,
            entries,
            &mut total,
            &mut kept,
        );
    }

    eprintln!(
        "RuTracker: {kept}/{total} kept in {:.1}s (total unique: {})",
        start.elapsed().as_secs_f64(),
        entries.len()
    );
}

/// Process a semicolon-delimited CSV (TPB/RuTracker format: #ADDED;HASH(B64);NAME;SIZE(BYTES))
fn process_semicolon_csv(
    path: &Path,
    source: &str,
    imdb_map: &HashMap<(String, i16), String>,
    hash_imdb: &HashMap<String, String>,
    entries: &mut HashMap<String, ProcessedEntry>,
    total: &mut u64,
    kept: &mut u64,
) {
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;

    let file = match std::fs::File::open(path) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("  Skipping {}: {}", path.display(), e);
            return;
        }
    };

    let reader = BufReader::with_capacity(512 * 1024, file);

    for line in reader.lines() {
        let line = match line {
            Ok(l) => l,
            Err(_) => continue,
        };

        if line.starts_with('#') {
            continue; // header
        }

        // Format: ADDED;HASH(B64);NAME;SIZE(BYTES)
        let parts: Vec<&str> = line.splitn(4, ';').collect();
        if parts.len() < 3 {
            continue;
        }

        let hash_b64 = parts[1];
        let name = parts[2];
        let size: Option<i64> = parts.get(3).and_then(|s| s.parse().ok());

        *total += 1;

        if name.is_empty() || hash_b64.is_empty() {
            continue;
        }

        // Decode hash
        let info_hash = if hash_b64.len() == 40 && hash_b64.chars().all(|c| c.is_ascii_hexdigit()) {
            hash_b64.to_lowercase()
        } else if let Ok(bytes) = STANDARD.decode(hash_b64) {
            if bytes.len() == 20 {
                bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
            } else {
                continue;
            }
        } else {
            continue;
        };

        // Skip if already have it
        if entries.contains_key(&info_hash) {
            continue;
        }

        if !is_wanted_content(name) {
            continue;
        }

        let media_kind = guess_media_kind(name);

        if let Some((parsed_title, year, quality_json, imdb_id, season, episodes)) =
            parse_and_enrich(name, media_kind, &info_hash, None, imdb_map, hash_imdb)
        {
            insert_entry(
                entries,
                info_hash,
                name.to_string(),
                parsed_title,
                year,
                quality_json,
                size,
                source,
                imdb_id,
                season,
                episodes,
            );
            *kept += 1;
        }
    }
}

fn process_dmm(
    seeds_dir: &Path,
    imdb_map: &HashMap<(String, i16), String>,
    hash_imdb: &HashMap<String, String>,
    entries: &mut HashMap<String, ProcessedEntry>,
) {
    let dmm_dir = seeds_dir.join("dmm-hashlists");
    if !dmm_dir.exists() {
        eprintln!("DMM: not found, skipping");
        return;
    }

    eprintln!("Processing DMM hashlists...");
    let start = Instant::now();

    let html_files: Vec<PathBuf> = std::fs::read_dir(&dmm_dir)
        .expect("read DMM dir")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.extension().and_then(|e| e.to_str()) == Some("html")
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n != "index.html")
        })
        .collect();

    let mut total = 0u64;
    let mut kept = 0u64;
    let mut files_ok = 0u64;

    for path in &html_files {
        let html = match std::fs::read_to_string(path) {
            Ok(h) => h,
            Err(_) => continue,
        };

        let fragment = match extract_dmm_fragment(&html) {
            Some(f) => f,
            None => continue,
        };

        let decompressed = match lz_str::decompress_from_encoded_uri_component(&fragment) {
            Some(d) => d,
            None => continue,
        };

        let json_str = match String::from_utf16(&decompressed) {
            Ok(s) => s,
            Err(_) => continue,
        };

        let torrents: Vec<DmmTorrent> = if json_str.starts_with('[') {
            serde_json::from_str(&json_str).unwrap_or_default()
        } else if json_str.starts_with('{') {
            serde_json::from_str::<DmmCollection>(&json_str)
                .map(|c| c.torrents)
                .unwrap_or_default()
        } else {
            continue;
        };

        files_ok += 1;

        for t in torrents {
            total += 1;
            let info_hash = t.hash.to_lowercase();
            if info_hash.len() != 40 || !info_hash.chars().all(|c| c.is_ascii_hexdigit()) {
                continue;
            }

            if entries.contains_key(&info_hash) {
                continue;
            }

            if !is_wanted_content(&t.filename) {
                continue;
            }

            let media_kind = guess_media_kind(&t.filename);
            if let Some((parsed_title, year, quality_json, imdb_id, season, episodes)) =
                parse_and_enrich(
                    &t.filename,
                    media_kind,
                    &info_hash,
                    None,
                    imdb_map,
                    hash_imdb,
                )
            {
                insert_entry(
                    entries,
                    info_hash,
                    t.filename,
                    parsed_title,
                    year,
                    quality_json,
                    t.bytes.map(|b| b as i64),
                    "dmm",
                    imdb_id,
                    season,
                    episodes,
                );
                kept += 1;
            }
        }
    }

    eprintln!(
        "DMM: {kept}/{total} kept from {files_ok}/{} files in {:.1}s (total unique: {})",
        html_files.len(),
        start.elapsed().as_secs_f64(),
        entries.len()
    );
}

#[derive(serde::Deserialize)]
struct DmmTorrent {
    filename: String,
    hash: String,
    bytes: Option<u64>,
}

#[derive(serde::Deserialize)]
struct DmmCollection {
    torrents: Vec<DmmTorrent>,
}

fn extract_dmm_fragment(html: &str) -> Option<String> {
    let marker = "debridmediamanager.com/hashlist#";
    let start = html.find(marker)?;
    let fragment_start = start + marker.len();
    let remaining = &html[fragment_start..];
    let end = remaining.find('"')?;
    Some(remaining[..end].to_string())
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn guess_media_kind(title: &str) -> &'static str {
    let lower = title.to_lowercase();
    if lower.contains(".s0")
        || lower.contains(".s1")
        || lower.contains(".s2")
        || lower.contains(".s3")
        || lower.contains("s01")
        || lower.contains("s02")
        || lower.contains("season")
        || lower.contains("episode")
        || lower.contains(".e0")
    {
        "tv"
    } else if lower.contains(".epub")
        || lower.contains(".mobi")
        || lower.contains("audiobook")
        || lower.contains("unabridged")
        || lower.contains(".m4b")
    {
        "ebooks"
    } else {
        "movies"
    }
}

// ---------------------------------------------------------------------------
// CSV output
// ---------------------------------------------------------------------------

/// Escape a string for PostgreSQL text COPY format.
/// Backslash is the escape character in COPY text format, so literal backslashes
/// must be doubled. Tabs and newlines replaced with spaces.
fn escape_for_pg_copy(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace(['\t', '\n'], " ")
        .replace(['\r', '\0'], "")
}

fn write_csv(path: &Path, entries: &HashMap<String, ProcessedEntry>) {
    let file = std::fs::File::create(path).expect("create output CSV");
    let mut writer = std::io::BufWriter::with_capacity(1024 * 1024, file);

    // Header matching torrent_hash_index table columns
    writeln!(
        writer,
        "info_hash\timdb_id\ttmdb_id\toriginal_title\tparsed_title\tyear\tquality\tsize\tmagnet_url\tsource\tseeders\tseason\tepisodes"
    )
    .expect("write header");

    let mut count = 0u64;
    for entry in entries.values() {
        // Tab-separated, with proper escaping for PostgreSQL COPY
        let imdb = entry.imdb_id.as_deref().unwrap_or("\\N");
        let tmdb = entry.tmdb_id.as_deref().unwrap_or("\\N");
        let year = entry
            .year
            .map(|y| y.to_string())
            .unwrap_or_else(|| "\\N".to_string());
        let quality = entry.quality_json.as_deref().unwrap_or("\\N");
        let size = entry
            .size
            .map(|s| s.to_string())
            .unwrap_or_else(|| "\\N".to_string());
        let season = entry
            .season
            .map(|s| s.to_string())
            .unwrap_or_else(|| "\\N".to_string());
        // Postgres COPY-text array literal: `{1,2,3}` or `\N` for NULL.
        let episodes = entry
            .episodes
            .as_ref()
            .map(|list| {
                let mut s = String::with_capacity(2 + list.len() * 4);
                s.push('{');
                for (i, e) in list.iter().enumerate() {
                    if i > 0 {
                        s.push(',');
                    }
                    s.push_str(&e.to_string());
                }
                s.push('}');
                s
            })
            .unwrap_or_else(|| "\\N".to_string());

        // Escape for PostgreSQL text COPY format: backslash is escape char
        let orig_title = escape_for_pg_copy(&entry.original_title);
        let parsed_title = escape_for_pg_copy(&entry.parsed_title);

        writeln!(
            writer,
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t\\N\t{}\t\\N\t{}\t{}",
            entry.info_hash,
            imdb,
            tmdb,
            orig_title,
            parsed_title,
            year,
            quality,
            size,
            entry.source,
            season,
            episodes,
        )
        .expect("write row");

        count += 1;
    }

    writer.flush().expect("flush CSV");
    eprintln!("Wrote {} rows to {}", count, path.display());
}
