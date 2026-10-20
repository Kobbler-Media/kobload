//! Backup and restore functionality for Kobload.
//!
//! Creates tar.gz archives containing:
//! - manifest.json with metadata
//! - database/ directory with CSV exports of configuration tables
//!
//! Unlike Kobbler, Kobload only backs up configuration data (no storage files).
//! Transient data like downloads, events, and matches are excluded.

use chrono::{DateTime, Utc};
use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use futures::TryStreamExt;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use sqlx::postgres::PgPoolCopyExt;
use std::collections::HashSet;
use std::fs::{self, File};
use std::io::{self, BufRead, BufReader, Write};
use std::path::{Component, Path};
use tar::{Archive, Builder};
use thiserror::Error;
use tracing::{debug, info, warn};

/// Manifest file included in every backup archive.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackupManifest {
    /// Manifest format version
    pub version: String,
    /// Kobload version that created this backup
    pub kobload_version: String,
    /// When the backup was created
    pub created_at: DateTime<Utc>,
    /// Number of database tables exported
    pub table_count: usize,
}

impl BackupManifest {
    pub fn new() -> Self {
        Self {
            version: "1.0".to_string(),
            kobload_version: env!("CARGO_PKG_VERSION").to_string(),
            created_at: Utc::now(),
            table_count: TABLES_IN_ORDER.len(),
        }
    }
}

impl Default for BackupManifest {
    fn default() -> Self {
        Self::new()
    }
}

/// Tables to backup in dependency order.
/// Configuration tables only - transient data is excluded.
pub const TABLES_IN_ORDER: &[&str] = &[
    // No dependencies
    "users",
    "media_type_config",
    // Depends on users
    "api_keys",
    "download_clients",
    // Quality profiles (independent)
    "quality_profiles",
    // Depends on media_type_config
    "handler_type_defaults",
    "indexer_sources",
    // Depends on indexer_sources
    "indexer_category_mappings",
    "watch_patterns",
    // Independent
    "status_webhooks",
    "app_settings",
    "watch_directories",
    // Depends on watch_patterns, indexer_sources (downloads FK is nullable/transient)
    "decision_log_sessions",
    // Depends on decision_log_sessions, indexer_sources
    "decision_log_provider_calls",
    // Depends on decision_log_sessions, decision_log_provider_calls
    "decision_log_releases",
    // Debrid providers (independent)
    "debrid_providers",
    // Depends on debrid_providers
    "debrid_availability_cache",
    "stream_release_outcomes",
    "stream_trait_outcomes",
    // Torrent hash index (independent, large — may be slow to backup)
    "torrent_hash_index",
    // Per-file metadata for multi-file torrents (depends on torrent_hash_index)
    "torrent_hash_files",
];

#[derive(Debug, Error)]
pub enum BackupError {
    #[error("IO error: {0}")]
    Io(#[from] io::Error),

    #[error("Database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("CSV error: {0}")]
    Csv(#[from] csv::Error),

    #[error("Invalid backup: {0}")]
    Invalid(String),

    #[error("Version mismatch: backup version {backup} is not compatible with {current}")]
    VersionMismatch { backup: String, current: String },
}

/// Result type for backup operations.
pub type BackupResult<T> = Result<T, BackupError>;

/// Statistics returned after a restore operation.
#[derive(Debug, Clone, Serialize)]
pub struct RestoreStats {
    pub tables_restored: usize,
}

/// Create a backup archive at the specified path.
pub async fn create_backup(pool: &PgPool, output_path: &Path) -> BackupResult<BackupManifest> {
    info!("Creating backup at {:?}", output_path);

    // Create temp directory for staging
    let temp_dir = tempfile::tempdir()?;
    let staging = temp_dir.path();

    // Create directory structure
    let db_dir = staging.join("database");
    fs::create_dir_all(&db_dir)?;

    // Export database tables
    for table in TABLES_IN_ORDER {
        export_table(pool, table, &db_dir).await?;
    }

    // Create manifest
    let manifest = BackupManifest::new();
    let manifest_path = staging.join("manifest.json");
    let manifest_json = serde_json::to_string_pretty(&manifest)?;
    fs::write(&manifest_path, &manifest_json)?;

    // Create tar.gz archive
    let output_file = File::create(output_path)?;
    let encoder = GzEncoder::new(output_file, Compression::default());
    let mut archive = Builder::new(encoder);

    // Add all files from staging directory
    archive.append_dir_all(".", staging)?;

    // Finish the archive
    let encoder = archive.into_inner()?;
    encoder.finish()?;

    info!("Backup created: {} tables", TABLES_IN_ORDER.len());

    Ok(manifest)
}

/// Export a single table to a CSV file using PostgreSQL COPY TO STDOUT.
async fn export_table(pool: &PgPool, table: &str, output_dir: &Path) -> BackupResult<()> {
    debug!("Exporting table: {}", table);

    let output_path = output_dir.join(format!("{}.csv", table));

    // Get column names from schema (validated source, not user input)
    let columns = get_table_columns(pool, table).await?;

    if columns.is_empty() {
        warn!("Table {} has no columns or doesn't exist, skipping", table);
        fs::write(&output_path, "")?;
        return Ok(());
    }

    let column_list = columns.join(", ");

    // Use PostgreSQL COPY TO STDOUT for high-performance export
    let copy_query = format!(
        "COPY {} ({}) TO STDOUT WITH (FORMAT CSV, HEADER)",
        table, column_list
    );

    let mut copy_stream = pool.copy_out_raw(&copy_query).await?;
    let mut output = File::create(&output_path)?;

    // Stream chunks directly to file
    while let Some(chunk) = copy_stream.try_next().await? {
        output.write_all(&chunk)?;
    }

    Ok(())
}

/// Get column names for a table from the database schema.
async fn get_table_columns(pool: &PgPool, table: &str) -> BackupResult<Vec<String>> {
    // Use parameterized query to safely get column names
    let columns: Vec<String> = sqlx::query_scalar!(
        "SELECT column_name::text as \"column_name!\" FROM information_schema.columns
         WHERE table_name = $1 ORDER BY ordinal_position",
        table,
    )
    .fetch_all(pool)
    .await?;

    Ok(columns)
}

/// Restore from a backup archive.
pub async fn restore_backup(pool: &PgPool, backup_path: &Path) -> BackupResult<RestoreStats> {
    info!("Restoring backup from {:?}", backup_path);

    // Create temp directory for extraction
    let temp_dir = tempfile::tempdir()?;
    let extract_dir = temp_dir.path();

    // Extract the archive safely (validate paths to prevent traversal attacks)
    let file = File::open(backup_path)?;
    let decoder = GzDecoder::new(BufReader::new(file));
    let mut archive = Archive::new(decoder);

    for entry in archive.entries()? {
        let mut entry = entry?;
        let path = entry.path()?;

        // Reject paths with parent directory references or absolute paths
        for component in path.components() {
            if matches!(component, Component::ParentDir) {
                return Err(BackupError::Invalid(
                    "path traversal detected in archive".to_string(),
                ));
            }
            if matches!(component, Component::RootDir | Component::Prefix(_)) {
                return Err(BackupError::Invalid(
                    "absolute path detected in archive".to_string(),
                ));
            }
        }

        entry.unpack_in(extract_dir)?;
    }

    // Read and validate manifest
    let manifest_path = extract_dir.join("manifest.json");
    if !manifest_path.exists() {
        return Err(BackupError::Invalid("Missing manifest.json".to_string()));
    }

    let manifest_content = fs::read_to_string(&manifest_path)?;
    let manifest: BackupManifest = serde_json::from_str(&manifest_content)?;

    // Check version compatibility
    if !manifest.version.starts_with("1.") {
        return Err(BackupError::VersionMismatch {
            backup: manifest.version,
            current: "1.0".to_string(),
        });
    }

    info!(
        "Restoring backup from {} (created {})",
        manifest.kobload_version, manifest.created_at
    );

    // Begin transaction
    let mut tx = pool.begin().await?;

    // Disable foreign key checks temporarily by truncating in reverse order
    // and then importing in forward order
    let tables_reversed: Vec<_> = TABLES_IN_ORDER.iter().rev().collect();

    for table in &tables_reversed {
        debug!("Truncating table: {}", table);
        let query = format!("TRUNCATE TABLE {} CASCADE", table);
        sqlx::query(sqlx::AssertSqlSafe(query))
            .execute(&mut *tx)
            .await?;
    }

    // Import tables in order
    let db_dir = extract_dir.join("database");
    let mut tables_restored = 0;

    for table in TABLES_IN_ORDER {
        let csv_path = db_dir.join(format!("{}.csv", table));
        if csv_path.exists() {
            import_table(pool, &mut tx, table, &csv_path).await?;
            tables_restored += 1;
        }
    }

    // Commit database changes
    tx.commit().await?;

    info!("Restore completed: {} tables", tables_restored);

    Ok(RestoreStats { tables_restored })
}

/// Import a table from a CSV file using PostgreSQL COPY FROM STDIN.
///
/// Handles schema evolution by only importing columns that exist in both
/// the backup CSV and the current database schema.
async fn import_table(
    pool: &PgPool,
    conn: &mut sqlx::PgConnection,
    table: &str,
    csv_path: &Path,
) -> BackupResult<()> {
    debug!("Importing table: {}", table);

    // Read CSV header to get backup columns
    let file = File::open(csv_path)?;
    let mut reader = BufReader::new(file);
    let mut header_line = String::new();
    reader.read_line(&mut header_line)?;

    let header_line = header_line.trim();
    if header_line.is_empty() {
        return Ok(()); // Empty file, nothing to import
    }

    let backup_columns: Vec<&str> = header_line.split(',').collect();
    if backup_columns.is_empty() {
        return Ok(());
    }

    // Get current schema columns (use pool for metadata query)
    let schema_columns = get_table_columns(pool, table).await?;
    let schema_set: HashSet<&str> = schema_columns.iter().map(|s| s.as_str()).collect();

    // Find columns that exist in both backup and current schema
    let mut import_columns: Vec<&str> = Vec::new();
    let mut import_indices: Vec<usize> = Vec::new();

    for (i, col) in backup_columns.iter().enumerate() {
        if schema_set.contains(col) {
            import_columns.push(col);
            import_indices.push(i);
        } else {
            debug!(
                "Table {}: skipping column '{}' (not in current schema)",
                table, col
            );
        }
    }

    if import_columns.is_empty() {
        warn!(
            "Table {}: no matching columns between backup and schema",
            table
        );
        return Ok(());
    }

    // Log schema evolution info
    let skipped_count = backup_columns.len() - import_columns.len();
    if skipped_count > 0 {
        info!(
            "Table {}: importing {} of {} columns ({} skipped)",
            table,
            import_columns.len(),
            backup_columns.len(),
            skipped_count
        );
    }

    let new_columns: Vec<&String> = schema_columns
        .iter()
        .filter(|c| !backup_columns.contains(&c.as_str()))
        .collect();
    if !new_columns.is_empty() {
        debug!(
            "Table {}: {} new columns will use defaults: {:?}",
            table,
            new_columns.len(),
            new_columns
        );
    }

    // Check if we need to filter columns or can stream directly
    let needs_filtering = import_indices.len() != backup_columns.len();

    let column_list = import_columns.join(", ");
    let copy_query = format!(
        "COPY {} ({}) FROM STDIN WITH (FORMAT CSV, HEADER)",
        table, column_list
    );

    // Use COPY FROM STDIN on the provided connection
    let mut copy_sink = conn.copy_in_raw(&copy_query).await?;

    if needs_filtering {
        // Filter CSV to only include matching columns
        let filtered = filter_csv_columns(csv_path, &import_indices)?;
        copy_sink.send(filtered).await?;
    } else {
        // Stream file directly - all columns match
        let content = fs::read(csv_path)?;
        copy_sink.send(content).await?;
    }

    copy_sink.finish().await?;

    Ok(())
}

/// Filter a CSV file to only include specified column indices.
/// Returns filtered CSV content as bytes (with header).
fn filter_csv_columns(csv_path: &Path, indices: &[usize]) -> BackupResult<Vec<u8>> {
    let file = File::open(csv_path)?;
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(true)
        .from_reader(file);

    let mut output = Vec::new();
    let mut writer = csv::WriterBuilder::new().from_writer(&mut output);

    // Get headers and write filtered header
    let headers = reader.headers()?.clone();
    let filtered_headers: Vec<&str> = indices.iter().filter_map(|&i| headers.get(i)).collect();
    writer.write_record(&filtered_headers)?;

    // Process data rows
    for result in reader.records() {
        let record = result?;
        let filtered: Vec<&str> = indices.iter().filter_map(|&i| record.get(i)).collect();
        writer.write_record(&filtered)?;
    }

    writer.flush()?;
    drop(writer);

    Ok(output)
}

/// Get the path where a backup file should be created.
#[allow(dead_code)]
pub fn backup_filename() -> String {
    let now = Utc::now();
    format!("kobload-backup-{}.tar.gz", now.format("%Y%m%d-%H%M%S"))
}
