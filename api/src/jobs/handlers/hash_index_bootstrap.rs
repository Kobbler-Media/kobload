//! Bootstrap import handler for the torrent hash index.
//!
//! Loads a pre-processed TSV (gzipped) into the hash index via
//! COPY to a temp table + upsert into the main table. Safe to run
//! at any time — merges with existing data, never overwrites.

use std::time::Instant;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::Value;
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;
use tracing::info;
use uuid::Uuid;

use crate::jobs::JobError;
use crate::jobs::worker::JobHandler;

#[derive(Debug, Deserialize)]
struct BootstrapPayload {
    file_path: String,
}

pub struct HashIndexBootstrapHandler {
    pool: PgPool,
}

impl HashIndexBootstrapHandler {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl JobHandler for HashIndexBootstrapHandler {
    fn job_type(&self) -> &'static str {
        "hash_index_bootstrap"
    }

    fn max_concurrent(&self) -> usize {
        1
    }

    async fn execute(
        &self,
        _job_id: Uuid,
        payload: Value,
        cancel: CancellationToken,
    ) -> Result<Value, JobError> {
        let payload: BootstrapPayload = serde_json::from_value(payload)
            .map_err(|e| JobError::ExecutionFailed(format!("Invalid payload: {e}")))?;

        let file_path = std::path::PathBuf::from(&payload.file_path);
        if !file_path.exists() {
            return Err(JobError::ExecutionFailed(format!(
                "Upload file not found: {}",
                file_path.display()
            )));
        }

        let start = Instant::now();
        info!(path = %file_path.display(), "Starting hash index bootstrap");

        nats_jobs::check_cancelled(&cancel)?;

        // Step 1: Decompress gzipped TSV to a temp file
        info!("Decompressing uploaded file...");
        let decompressed_path = file_path.with_extension("tsv");
        decompress_gz(&file_path, &decompressed_path)?;

        let line_count = count_lines(&decompressed_path)?;
        info!(lines = line_count, "Decompressed TSV");

        nats_jobs::check_cancelled(&cancel)?;

        // Acquire a single connection for the entire temp table lifecycle.
        // Temp tables are session-scoped — using the pool directly would
        // dispatch each statement to a different connection.
        let mut conn =
            self.pool.acquire().await.map_err(|e| {
                JobError::ExecutionFailed(format!("Failed to acquire connection: {e}"))
            })?;

        // Step 2: Create temp table
        info!("Creating temp table...");
        sqlx::query("CREATE TEMP TABLE hash_import (LIKE torrent_hash_index INCLUDING DEFAULTS)")
            .execute(&mut *conn)
            .await
            .map_err(|e| JobError::ExecutionFailed(format!("Failed to create temp table: {e}")))?;

        // Step 3: COPY into temp table
        // Since we can't COPY FROM a file path on the app server (DB might be remote),
        // we stream the data via the COPY protocol using sqlx's copy_in.
        info!("Loading data into temp table via COPY...");
        let rows_copied = copy_tsv_to_temp_table(&mut conn, &decompressed_path).await?;
        info!(rows_copied, "Data loaded into temp table");

        nats_jobs::check_cancelled(&cancel)?;

        // Step 4: Upsert from temp table into main table with COALESCE enrichment
        info!("Upserting into main table...");
        let result = sqlx::query(
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
        .execute(&mut *conn)
        .await
        .map_err(|e| JobError::ExecutionFailed(format!("Upsert failed: {e}")))?;

        let rows_affected = result.rows_affected();
        info!(rows_affected, "Upsert complete");

        // Drop temp table and release connection before ANALYZE
        sqlx::query("DROP TABLE IF EXISTS hash_import")
            .execute(&mut *conn)
            .await
            .ok();
        drop(conn);

        // Step 5: ANALYZE for fresh statistics (can use pool — no temp table dependency)
        info!("Running ANALYZE...");
        sqlx::raw_sql("ANALYZE torrent_hash_index")
            .execute(&self.pool)
            .await
            .map_err(|e| JobError::ExecutionFailed(format!("ANALYZE failed: {e}")))?;

        // Step 6: Cleanup decompressed file (keep the original .gz)
        let _ = std::fs::remove_file(&decompressed_path);

        let elapsed = start.elapsed();
        info!(
            rows_affected,
            elapsed_secs = elapsed.as_secs(),
            "Hash index bootstrap complete"
        );

        // Get final count
        let total = sqlx::query_scalar!("SELECT COUNT(*) FROM torrent_hash_index")
            .fetch_one(&self.pool)
            .await
            .map_err(|e| JobError::ExecutionFailed(format!("Count failed: {e}")))?
            .unwrap_or(0);

        Ok(serde_json::json!({
            "rows_imported": rows_copied,
            "rows_upserted": rows_affected,
            "total_in_index": total,
            "elapsed_seconds": elapsed.as_secs(),
        }))
    }
}

/// Decompress a .gz file to a destination path.
fn decompress_gz(src: &std::path::Path, dst: &std::path::Path) -> Result<(), JobError> {
    let file = std::fs::File::open(src)
        .map_err(|e| JobError::ExecutionFailed(format!("Failed to open gz file: {e}")))?;

    let mut decoder = flate2::read::GzDecoder::new(file);
    let mut out = std::fs::File::create(dst).map_err(|e| {
        JobError::ExecutionFailed(format!("Failed to create decompressed file: {e}"))
    })?;

    std::io::copy(&mut decoder, &mut out)
        .map_err(|e| JobError::ExecutionFailed(format!("Decompression failed: {e}")))?;

    Ok(())
}

/// Count lines in a file (for progress reporting).
fn count_lines(path: &std::path::Path) -> Result<u64, JobError> {
    let file = std::fs::File::open(path)
        .map_err(|e| JobError::ExecutionFailed(format!("Failed to open file: {e}")))?;
    let reader = std::io::BufReader::new(file);
    Ok(std::io::BufRead::lines(reader).count() as u64)
}

/// Stream a TSV file into the hash_import temp table via PostgreSQL COPY protocol.
/// Streams from disk in chunks — doesn't load the full file into memory.
/// Must be called on the same connection that created the temp table.
async fn copy_tsv_to_temp_table(
    conn: &mut sqlx::PgConnection,
    tsv_path: &std::path::Path,
) -> Result<u64, JobError> {
    use tokio::io::{AsyncBufReadExt, BufReader};

    let file = tokio::fs::File::open(tsv_path)
        .await
        .map_err(|e| JobError::ExecutionFailed(format!("Failed to open TSV: {e}")))?;

    let mut reader = BufReader::with_capacity(8 * 1024 * 1024, file);

    // Skip header line
    let mut header = String::new();
    reader
        .read_line(&mut header)
        .await
        .map_err(|e| JobError::ExecutionFailed(format!("Failed to read header: {e}")))?;

    // Start COPY
    let mut copy_in = conn
        .copy_in_raw(
            "COPY hash_import(info_hash, imdb_id, tmdb_id, original_title, parsed_title, year, quality, size, magnet_url, source, seeders, season, episodes) FROM STDIN WITH (FORMAT text, NULL '\\N')",
        )
        .await
        .map_err(|e| JobError::ExecutionFailed(format!("COPY IN start failed: {e}")))?;

    // Stream in chunks
    let mut buf = vec![0u8; 8 * 1024 * 1024]; // 8MB buffer
    loop {
        let n = tokio::io::AsyncReadExt::read(&mut reader, &mut buf)
            .await
            .map_err(|e| JobError::ExecutionFailed(format!("Read error: {e}")))?;
        if n == 0 {
            break;
        }
        copy_in
            .send(&buf[..n])
            .await
            .map_err(|e| JobError::ExecutionFailed(format!("COPY IN send failed: {e}")))?;
    }

    let rows = copy_in
        .finish()
        .await
        .map_err(|e| JobError::ExecutionFailed(format!("COPY IN finish failed: {e}")))?;

    Ok(rows)
}
