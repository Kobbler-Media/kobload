//! Bulk import handler for the torrent hash index.
//!
//! Imports torrent metadata from bulk dump sources (RARBG, TPB, DMM)
//! into the `torrent_hash_index` table. Designed for one-time seeding and
//! periodic re-imports.

use std::path::PathBuf;
use std::time::Instant;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use uuid::Uuid;

use crate::jobs::JobError;
use crate::jobs::worker::JobHandler;
use crate::parsing::{guess_media_kind, parse_release};
use crate::repositories::hash_index::{self, HashIndexEntry};

/// Job payload.
#[derive(Debug, Deserialize)]
struct ImportPayload {
    source: String, // "rarbg", "tpb", "dmm", "all"
}

/// Handler for hash_index_import jobs.
pub struct HashIndexImportHandler {
    pool: PgPool,
}

impl HashIndexImportHandler {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl JobHandler for HashIndexImportHandler {
    fn job_type(&self) -> &'static str {
        "hash_index_import"
    }

    fn max_concurrent(&self) -> usize {
        1 // Only one import at a time
    }

    async fn execute(
        &self,
        _job_id: Uuid,
        payload: Value,
        cancel: CancellationToken,
    ) -> Result<Value, JobError> {
        let payload: ImportPayload = serde_json::from_value(payload)
            .map_err(|e| JobError::ExecutionFailed(format!("Invalid payload: {e}")))?;

        let seeds_dir = find_seeds_dir();
        info!(source = %payload.source, seeds_dir = %seeds_dir.display(), "Starting hash index import");

        let start = Instant::now();

        // Drop indexes once before all imports
        info!("Dropping secondary indexes for bulk import");
        hash_index::drop_secondary_indexes(&self.pool)
            .await
            .map_err(|e| JobError::ExecutionFailed(format!("Failed to drop indexes: {e}")))?;

        let mut total_imported = 0u64;

        let sources: Vec<&str> = if payload.source == "all" {
            vec!["rarbg", "magnetdb", "tpb", "dmm"]
        } else {
            vec![payload.source.as_str()]
        };

        for source in &sources {
            nats_jobs::check_cancelled(&cancel)?;

            let count = match *source {
                "rarbg" => import_rarbg(&self.pool, &seeds_dir, &cancel).await?,
                "magnetdb" => import_magnetdb(&self.pool, &seeds_dir, &cancel).await?,
                "tpb" => import_tpb(&self.pool, &seeds_dir, &cancel).await?,
                "dmm" => import_dmm(&self.pool, &seeds_dir, &cancel).await?,
                other => {
                    warn!(source = other, "Unknown import source, skipping");
                    0
                }
            };

            total_imported += count;
            info!(source, count, "Source import complete");
        }

        // Recreate indexes after all imports
        info!("Recreating secondary indexes");
        hash_index::recreate_secondary_indexes(&self.pool)
            .await
            .map_err(|e| JobError::ExecutionFailed(format!("Failed to recreate indexes: {e}")))?;

        let elapsed = start.elapsed();
        info!(
            total_imported,
            elapsed_secs = elapsed.as_secs(),
            "Hash index import complete"
        );

        Ok(serde_json::json!({
            "total_imported": total_imported,
            "elapsed_seconds": elapsed.as_secs(),
        }))
    }
}

/// Find the seeds directory. Checks both the local dev path and a Docker volume mount.
fn find_seeds_dir() -> PathBuf {
    // In development, seeds are at data/seeds/ relative to the repo root.
    // In Docker, they might be at /data/seeds/ (volume mount).
    let candidates = [
        PathBuf::from("data/seeds"),
        PathBuf::from("/data/seeds"),
        PathBuf::from("/app/data/seeds"),
    ];
    for path in &candidates {
        if path.exists() {
            return path.clone();
        }
    }
    // Default to the first candidate even if it doesn't exist
    candidates[0].clone()
}

// ---------------------------------------------------------------------------
// RARBG import
// ---------------------------------------------------------------------------

/// Import from the RARBG SQLite dump.
async fn import_rarbg(
    pool: &PgPool,
    seeds_dir: &std::path::Path,
    cancel: &CancellationToken,
) -> Result<u64, JobError> {
    let db_path = seeds_dir.join("rarbg_db.sqlite");
    if !db_path.exists() {
        return Err(JobError::ExecutionFailed(format!(
            "RARBG dump not found at {}",
            db_path.display()
        )));
    }

    info!(path = %db_path.display(), "Opening RARBG SQLite dump");

    // rusqlite is sync, so we run it in a blocking task
    let pool = pool.clone();
    let cancel = cancel.clone();
    tokio::task::spawn_blocking(move || {
        let conn = rusqlite::Connection::open_with_flags(
            &db_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .map_err(|e| JobError::ExecutionFailed(format!("Failed to open RARBG SQLite: {e}")))?;

        // Discover table structure
        let table_name = find_rarbg_table(&conn)?;
        info!(table = %table_name, "Found RARBG data table");

        let mut stmt = conn
            .prepare(&format!(
                "SELECT hash, title, dt, cat, imdb, size FROM \"{table_name}\""
            ))
            .map_err(|e| {
                JobError::ExecutionFailed(format!("Failed to prepare RARBG query: {e}"))
            })?;

        let mut batch: Vec<HashIndexEntry> = Vec::with_capacity(5000);
        let mut total = 0u64;
        let rows_iter = stmt
            .query_map([], |row| {
                let hash: String = row.get(0)?;
                let title: String = row.get(1)?;
                let _dt: Option<String> = row.get(2)?;
                let cat: Option<String> = row.get(3)?;
                let imdb: Option<String> = row.get(4)?;
                let size: Option<i64> = row.get(5)?;
                Ok((hash, title, cat, imdb, size))
            })
            .map_err(|e| JobError::ExecutionFailed(format!("RARBG query failed: {e}")))?;

        let rt = tokio::runtime::Handle::current();

        for row_result in rows_iter {
            if cancel.is_cancelled() {
                return Err(JobError::Cancelled);
            }

            let (hash, title, cat, imdb, size) = match row_result {
                Ok(r) => r,
                Err(e) => {
                    warn!(error = %e, "Skipping RARBG row with read error");
                    continue;
                }
            };

            // Determine media kind from RARBG category
            let media_kind = rarbg_cat_to_media_kind(cat.as_deref());

            // Parse quality from release title
            let parsed = parse_release(&title, media_kind);
            let quality_json = serde_json::to_value(&parsed.quality).ok();
            let year = match &parsed.media {
                crate::parsing::ParsedMedia::Movie(m) => m.year.and_then(|y| i16::try_from(y).ok()),
                _ => None,
            };

            // Normalize IMDB ID
            let imdb_id = imdb.filter(|id| id.starts_with("tt") && id.len() > 2);

            // Always parse as TV for S/E — `parsed` above may have been
            // classified as "movies" by the category/guess heuristic, which
            // would skip S/E detection on edge-case TV titles.
            let (season, episodes) = hash_index::derive_season_episodes(&title);

            batch.push(HashIndexEntry {
                info_hash: hash.to_lowercase(),
                imdb_id,
                tmdb_id: None,
                original_title: title,
                parsed_title: Some(parsed.title),
                year,
                quality: quality_json,
                size,
                magnet_url: None,
                source: "rarbg_dump".to_string(),
                seeders: None,
                season,
                episodes,
            });

            if batch.len() >= 5000 {
                let count = rt
                    .block_on(hash_index::bulk_insert(&pool, &batch))
                    .map_err(|e| JobError::ExecutionFailed(format!("Bulk insert failed: {e}")))?;
                total += count;
                batch.clear();

                if total.is_multiple_of(100_000) {
                    info!(total, "RARBG import progress");
                }
            }
        }

        // Final batch
        if !batch.is_empty() {
            let count = rt
                .block_on(hash_index::bulk_insert(&pool, &batch))
                .map_err(|e| JobError::ExecutionFailed(format!("Final bulk insert failed: {e}")))?;
            total += count;
        }

        info!(total, "RARBG import complete");
        Ok(total)
    })
    .await
    .map_err(|e| JobError::ExecutionFailed(format!("RARBG import task panicked: {e}")))?
}

/// Find the main data table in the RARBG SQLite dump.
fn find_rarbg_table(conn: &rusqlite::Connection) -> Result<String, JobError> {
    let tables = list_sqlite_tables(conn)?;
    info!(tables = ?tables, "RARBG SQLite tables");

    // Common table names in RARBG dumps
    for candidate in &["items", "torrents", "rarbg", "data"] {
        if tables.iter().any(|t| t == *candidate) {
            return Ok(candidate.to_string());
        }
    }

    // Fall back to first non-system table
    tables
        .into_iter()
        .find(|t| !t.starts_with("sqlite_"))
        .ok_or_else(|| {
            JobError::ExecutionFailed("No data table found in RARBG SQLite dump".to_string())
        })
}

/// Map RARBG category strings to media_kind for parsing.
fn rarbg_cat_to_media_kind(cat: Option<&str>) -> &'static str {
    match cat {
        Some(c) if c.contains("movie") => "movies",
        Some(c) if c.contains("tv") => "tv",
        Some(c) if c.contains("music") => "music",
        Some(c) if c.contains("game") => "games",
        Some(c) if c.contains("software") || c.contains("app") => "software",
        Some(c) if c.contains("ebook") || c.contains("book") => "ebooks",
        Some(c) if c.contains("xxx") => "xxx",
        _ => "movies", // default to movies for unknown categories
    }
}

// ---------------------------------------------------------------------------
// MagnetDB import
// ---------------------------------------------------------------------------

/// Import from the MagnetDB SQLite dump.
/// Expects `magnetdb_public.sqlite3` in the seeds directory (after decompressing .tar.zst).
async fn import_magnetdb(
    pool: &PgPool,
    seeds_dir: &std::path::Path,
    cancel: &CancellationToken,
) -> Result<u64, JobError> {
    // Look for the SQLite file (might be in a subdirectory after tar extraction)
    let candidates = [
        seeds_dir.join("magnetdb_public.sqlite3"),
        seeds_dir.join("magnetdb/magnetdb_public.sqlite3"),
        seeds_dir.join("MagnetDB_2024/magnetdb_public.sqlite3"),
    ];

    let db_path = candidates.iter().find(|p| p.exists()).ok_or_else(|| {
        JobError::ExecutionFailed(format!(
            "MagnetDB SQLite dump not found. Checked: {}",
            candidates
                .iter()
                .map(|p| p.display().to_string())
                .collect::<Vec<_>>()
                .join(", ")
        ))
    })?;

    info!(path = %db_path.display(), "Opening MagnetDB SQLite dump");

    let pool = pool.clone();
    let cancel = cancel.clone();
    let db_path = db_path.clone();
    tokio::task::spawn_blocking(move || {
        let conn = rusqlite::Connection::open_with_flags(
            &db_path,
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .map_err(|e| JobError::ExecutionFailed(format!("Failed to open MagnetDB SQLite: {e}")))?;

        // MagnetDB schema: torrents table with info_hash, name, size, etc.
        // matchedVideos may have IMDB mappings
        // Discover available tables
        let tables = list_sqlite_tables(&conn)?;
        info!(tables = ?tables, "MagnetDB tables");

        // Try the main torrents table first
        let torrent_table = tables
            .iter()
            .find(|t| t.contains("torrent"))
            .or_else(|| tables.iter().find(|t| !t.starts_with("sqlite_")))
            .ok_or_else(|| {
                JobError::ExecutionFailed("No suitable table found in MagnetDB dump".to_string())
            })?;

        info!(table = %torrent_table, "Using MagnetDB table");

        // Query columns to understand schema
        let columns = list_sqlite_columns(&conn, torrent_table)?;
        info!(columns = ?columns, "MagnetDB columns");

        // Build query based on available columns
        let hash_col = columns
            .iter()
            .find(|c| c.contains("hash") || c.contains("info_hash"))
            .map(|s| s.as_str())
            .unwrap_or("info_hash");
        let name_col = columns
            .iter()
            .find(|c| c == &"name" || c == &"title")
            .map(|s| s.as_str())
            .unwrap_or("name");
        let size_col = if columns.iter().any(|c| c == "size") {
            Some("size")
        } else {
            None
        };

        let query = if let Some(size) = size_col {
            format!("SELECT \"{hash_col}\", \"{name_col}\", \"{size}\" FROM \"{torrent_table}\"")
        } else {
            format!("SELECT \"{hash_col}\", \"{name_col}\", NULL FROM \"{torrent_table}\"")
        };

        let mut stmt = conn.prepare(&query).map_err(|e| {
            JobError::ExecutionFailed(format!("Failed to prepare MagnetDB query: {e}"))
        })?;

        let mut batch: Vec<HashIndexEntry> = Vec::with_capacity(5000);
        let mut total = 0u64;
        let rt = tokio::runtime::Handle::current();

        let rows_iter = stmt
            .query_map([], |row| {
                let hash: String = row.get(0)?;
                let name: String = row.get(1)?;
                let size: Option<i64> = row.get(2)?;
                Ok((hash, name, size))
            })
            .map_err(|e| JobError::ExecutionFailed(format!("MagnetDB query failed: {e}")))?;

        for row_result in rows_iter {
            if cancel.is_cancelled() {
                return Err(JobError::Cancelled);
            }

            let (hash, name, size) = match row_result {
                Ok(r) => r,
                Err(e) => {
                    warn!(error = %e, "Skipping MagnetDB row");
                    continue;
                }
            };

            // Normalize hash to lowercase hex
            let info_hash = hash.to_lowercase();
            if info_hash.len() != 40 || !info_hash.chars().all(|c| c.is_ascii_hexdigit()) {
                continue;
            }

            let media_kind = guess_media_kind(&name);
            let parsed = parse_release(&name, media_kind);
            let quality_json = serde_json::to_value(&parsed.quality).ok();
            let year = match &parsed.media {
                crate::parsing::ParsedMedia::Movie(m) => m.year.and_then(|y| i16::try_from(y).ok()),
                _ => None,
            };

            // Always parse as TV for S/E — `parsed` above may have been
            // classified as "movies" by the category/guess heuristic, which
            // would skip S/E detection on edge-case TV titles.
            let (season, episodes) = hash_index::derive_season_episodes(&name);

            batch.push(HashIndexEntry {
                info_hash,
                imdb_id: None, // MagnetDB public dump may not have IMDB mappings inline
                tmdb_id: None,
                original_title: name,
                parsed_title: Some(parsed.title),
                year,
                quality: quality_json,
                size,
                magnet_url: None,
                source: "magnetdb".to_string(),
                seeders: None,
                season,
                episodes,
            });

            if batch.len() >= 5000 {
                let count = rt
                    .block_on(hash_index::bulk_insert(&pool, &batch))
                    .map_err(|e| JobError::ExecutionFailed(format!("Bulk insert failed: {e}")))?;
                total += count;
                batch.clear();

                if total.is_multiple_of(100_000) {
                    info!(total, "MagnetDB import progress");
                }
            }
        }

        if !batch.is_empty() {
            let count = rt
                .block_on(hash_index::bulk_insert(&pool, &batch))
                .map_err(|e| JobError::ExecutionFailed(format!("Final bulk insert failed: {e}")))?;
            total += count;
        }

        info!(total, "MagnetDB import complete");
        Ok(total)
    })
    .await
    .map_err(|e| JobError::ExecutionFailed(format!("MagnetDB import task panicked: {e}")))?
}

/// List tables in a SQLite database.
fn list_sqlite_tables(conn: &rusqlite::Connection) -> Result<Vec<String>, JobError> {
    let mut stmt = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name")
        .map_err(|e| JobError::ExecutionFailed(format!("Failed to list tables: {e}")))?;

    stmt.query_map([], |row| row.get(0))
        .map_err(|e| JobError::ExecutionFailed(format!("Failed to iterate tables: {e}")))?
        .filter_map(|r| r.ok())
        .collect::<Vec<String>>()
        .pipe(Ok)
}

/// List columns in a SQLite table.
fn list_sqlite_columns(conn: &rusqlite::Connection, table: &str) -> Result<Vec<String>, JobError> {
    let mut stmt = conn
        .prepare(&format!("PRAGMA table_info(\"{table}\")"))
        .map_err(|e| JobError::ExecutionFailed(format!("Failed to get table info: {e}")))?;

    stmt.query_map([], |row| row.get::<_, String>(1))
        .map_err(|e| JobError::ExecutionFailed(format!("Failed to iterate columns: {e}")))?
        .filter_map(|r| r.ok())
        .collect::<Vec<String>>()
        .pipe(Ok)
}

/// Extension trait for piping values (like Rust's proposed `.pipe()`).
trait Pipe: Sized {
    fn pipe<R>(self, f: impl FnOnce(Self) -> R) -> R {
        f(self)
    }
}

impl<T> Pipe for T {}

// ---------------------------------------------------------------------------
// TPB (The Pirate Bay) import
// ---------------------------------------------------------------------------

/// Import from TPB CSV dumps.
async fn import_tpb(
    pool: &PgPool,
    seeds_dir: &std::path::Path,
    cancel: &CancellationToken,
) -> Result<u64, JobError> {
    let csv_files: Vec<PathBuf> = std::fs::read_dir(seeds_dir)
        .map_err(|e| JobError::ExecutionFailed(format!("Failed to read seeds dir: {e}")))?
        .filter_map(|entry| entry.ok())
        .map(|e| e.path())
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("piratebay") && n.ends_with(".csv"))
        })
        .collect();

    if csv_files.is_empty() {
        return Err(JobError::ExecutionFailed(
            "No TPB CSV files found in seeds directory".to_string(),
        ));
    }

    let mut total = 0u64;

    for csv_path in &csv_files {
        info!(path = %csv_path.display(), "Importing TPB CSV");
        let count = import_tpb_file(pool, csv_path, cancel).await?;
        total += count;
    }

    info!(total, "TPB import complete");
    Ok(total)
}

async fn import_tpb_file(
    pool: &PgPool,
    csv_path: &std::path::Path,
    cancel: &CancellationToken,
) -> Result<u64, JobError> {
    let pool = pool.clone();
    let cancel = cancel.clone();
    let csv_path = csv_path.to_path_buf();

    tokio::task::spawn_blocking(move || {
        use base64::Engine;
        use base64::engine::general_purpose::STANDARD;

        let mut reader = csv::ReaderBuilder::new()
            .has_headers(false)
            .delimiter(b';')
            .from_path(&csv_path)
            .map_err(|e| JobError::ExecutionFailed(format!("Failed to open TPB CSV: {e}")))?;

        let mut batch: Vec<HashIndexEntry> = Vec::with_capacity(5000);
        let mut total = 0u64;
        let rt = tokio::runtime::Handle::current();

        for result in reader.records() {
            if cancel.is_cancelled() {
                return Err(JobError::Cancelled);
            }

            let record = match result {
                Ok(r) => r,
                Err(e) => {
                    warn!(error = %e, "Skipping TPB CSV row with parse error");
                    continue;
                }
            };

            // TPB CSV format: hash_base64;title;size_bytes
            // Some dumps use different column orders, so we try to be flexible
            if record.len() < 2 {
                continue;
            }

            let hash_field = record.get(0).unwrap_or("");
            let title = record.get(1).unwrap_or("").to_string();

            if title.is_empty() || hash_field.is_empty() {
                continue;
            }

            // Decode base64 hash to hex
            let info_hash =
                if hash_field.len() == 40 && hash_field.chars().all(|c| c.is_ascii_hexdigit()) {
                    // Already hex
                    hash_field.to_lowercase()
                } else if let Ok(bytes) = STANDARD.decode(hash_field) {
                    // Base64-encoded — convert to hex
                    bytes.iter().map(|b| format!("{b:02x}")).collect::<String>()
                } else {
                    continue; // Can't decode hash
                };

            if info_hash.len() != 40 {
                continue;
            }

            let size: Option<i64> = record.get(2).and_then(|s| s.parse().ok());

            // Guess media kind from title
            let media_kind = guess_media_kind(&title);
            let parsed = parse_release(&title, media_kind);
            let quality_json = serde_json::to_value(&parsed.quality).ok();
            let year = match &parsed.media {
                crate::parsing::ParsedMedia::Movie(m) => m.year.and_then(|y| i16::try_from(y).ok()),
                _ => None,
            };

            // Always parse as TV for S/E — `parsed` above may have been
            // classified as "movies" by the category/guess heuristic, which
            // would skip S/E detection on edge-case TV titles.
            let (season, episodes) = hash_index::derive_season_episodes(&title);

            batch.push(HashIndexEntry {
                info_hash,
                imdb_id: None,
                tmdb_id: None,
                original_title: title,
                parsed_title: Some(parsed.title),
                year,
                quality: quality_json,
                size,
                magnet_url: None,
                source: "tpb".to_string(),
                seeders: None,
                season,
                episodes,
            });

            if batch.len() >= 5000 {
                let count = rt
                    .block_on(hash_index::bulk_insert(&pool, &batch))
                    .map_err(|e| JobError::ExecutionFailed(format!("Bulk insert failed: {e}")))?;
                total += count;
                batch.clear();

                if total.is_multiple_of(100_000) {
                    info!(total, "TPB import progress");
                }
            }
        }

        if !batch.is_empty() {
            let count = rt
                .block_on(hash_index::bulk_insert(&pool, &batch))
                .map_err(|e| JobError::ExecutionFailed(format!("Final bulk insert failed: {e}")))?;
            total += count;
        }

        Ok(total)
    })
    .await
    .map_err(|e| JobError::ExecutionFailed(format!("TPB import task panicked: {e}")))?
}

// ---------------------------------------------------------------------------
// DMM (Debrid Media Manager) hashlists import
// ---------------------------------------------------------------------------

/// Import from DMM hashlist HTML files.
async fn import_dmm(
    pool: &PgPool,
    seeds_dir: &std::path::Path,
    cancel: &CancellationToken,
) -> Result<u64, JobError> {
    let dmm_dir = seeds_dir.join("dmm-hashlists");
    if !dmm_dir.exists() {
        return Err(JobError::ExecutionFailed(format!(
            "DMM hashlists directory not found at {}",
            dmm_dir.display()
        )));
    }

    let pool = pool.clone();
    let cancel = cancel.clone();

    tokio::task::spawn_blocking(move || {
        let html_files: Vec<PathBuf> = std::fs::read_dir(&dmm_dir)
            .map_err(|e| JobError::ExecutionFailed(format!("Failed to read DMM dir: {e}")))?
            .filter_map(|entry| entry.ok())
            .map(|e| e.path())
            .filter(|p| {
                p.extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e == "html")
                    && p.file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| n != "index.html")
            })
            .collect();

        info!(file_count = html_files.len(), "Found DMM hashlist files");

        let mut batch: Vec<HashIndexEntry> = Vec::with_capacity(5000);
        let mut total = 0u64;
        let mut files_processed = 0usize;
        let rt = tokio::runtime::Handle::current();

        for html_path in &html_files {
            if cancel.is_cancelled() {
                return Err(JobError::Cancelled);
            }

            match parse_dmm_hashlist(html_path) {
                Ok(entries) => {
                    for entry in entries {
                        batch.push(entry);

                        if batch.len() >= 5000 {
                            let count = rt
                                .block_on(hash_index::bulk_insert(&pool, &batch))
                                .map_err(|e| {
                                    JobError::ExecutionFailed(format!("Bulk insert failed: {e}"))
                                })?;
                            total += count;
                            batch.clear();
                        }
                    }
                }
                Err(e) => {
                    // Non-fatal — skip malformed files
                    warn!(
                        path = %html_path.display(),
                        error = %e,
                        "Skipping malformed DMM hashlist"
                    );
                }
            }

            files_processed += 1;
            if files_processed.is_multiple_of(1000) {
                info!(files_processed, total, "DMM import progress");
            }
        }

        if !batch.is_empty() {
            let count = rt
                .block_on(hash_index::bulk_insert(&pool, &batch))
                .map_err(|e| JobError::ExecutionFailed(format!("Final bulk insert failed: {e}")))?;
            total += count;
        }

        info!(total, files_processed, "DMM import complete");
        Ok(total)
    })
    .await
    .map_err(|e| JobError::ExecutionFailed(format!("DMM import task panicked: {e}")))?
}

/// DMM hashlist torrent entry.
#[derive(Debug, Deserialize)]
struct DmmTorrent {
    filename: String,
    hash: String,
    bytes: Option<u64>,
}

/// DMM hashlist collection variant (object with title + torrents array).
#[derive(Debug, Deserialize)]
struct DmmCollection {
    #[allow(dead_code)]
    title: Option<String>,
    torrents: Vec<DmmTorrent>,
}

/// Parse a single DMM hashlist HTML file into HashIndexEntry entries.
fn parse_dmm_hashlist(path: &std::path::Path) -> Result<Vec<HashIndexEntry>, String> {
    let html = std::fs::read_to_string(path).map_err(|e| format!("Read error: {e}"))?;

    // Extract the fragment from the iframe src="...#COMPRESSED_DATA"
    let fragment = extract_dmm_fragment(&html)?;

    // Decompress using lz-string (returns Vec<u16>, convert to String)
    let decompressed = lz_str::decompress_from_encoded_uri_component(&fragment)
        .ok_or_else(|| "lz-string decompression failed".to_string())?;

    let json_str =
        String::from_utf16(&decompressed).map_err(|e| format!("UTF-16 decode error: {e}"))?;

    // Parse as either array of torrents or collection object
    let torrents: Vec<DmmTorrent> = if json_str.starts_with('[') {
        serde_json::from_str(&json_str).map_err(|e| format!("JSON parse error: {e}"))?
    } else if json_str.starts_with('{') {
        let collection: DmmCollection =
            serde_json::from_str(&json_str).map_err(|e| format!("JSON parse error: {e}"))?;
        collection.torrents
    } else {
        return Err("Unknown DMM JSON format".to_string());
    };

    let entries = torrents
        .into_iter()
        .filter(|t| t.hash.len() == 40 && t.hash.chars().all(|c| c.is_ascii_hexdigit()))
        .map(|t| {
            let media_kind = guess_media_kind(&t.filename);
            let parsed = parse_release(&t.filename, media_kind);
            let quality_json = serde_json::to_value(&parsed.quality).ok();
            let year = match &parsed.media {
                crate::parsing::ParsedMedia::Movie(m) => m.year.and_then(|y| i16::try_from(y).ok()),
                _ => None,
            };

            // Always parse as TV for S/E — `parsed` above may have been
            // classified as "movies" by the category/guess heuristic, which
            // would skip S/E detection on edge-case TV titles.
            let (season, episodes) = hash_index::derive_season_episodes(&t.filename);

            HashIndexEntry {
                info_hash: t.hash.to_lowercase(),
                imdb_id: None,
                tmdb_id: None,
                original_title: t.filename,
                parsed_title: Some(parsed.title),
                year,
                quality: quality_json,
                size: t.bytes.map(|b| b as i64),
                magnet_url: None,
                source: "dmm".to_string(),
                seeders: None,
                season,
                episodes,
            }
        })
        .collect();

    Ok(entries)
}

/// Extract the lz-string compressed fragment from a DMM hashlist HTML file.
fn extract_dmm_fragment(html: &str) -> Result<String, String> {
    // Look for: src="https://debridmediamanager.com/hashlist#COMPRESSED_DATA"
    let marker = "debridmediamanager.com/hashlist#";
    let start = html
        .find(marker)
        .ok_or_else(|| "No DMM hashlist marker found in HTML".to_string())?;
    let fragment_start = start + marker.len();

    // Find the closing quote
    let remaining = &html[fragment_start..];
    let end = remaining
        .find('"')
        .ok_or_else(|| "No closing quote after DMM fragment".to_string())?;

    Ok(remaining[..end].to_string())
}
