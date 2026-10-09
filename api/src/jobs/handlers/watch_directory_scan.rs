//! Watch directory scan handler.
//!
//! Walks a watch directory, discovers media files, checks for duplicates
//! against the downloads table, and stores parsed filename interpretations.

use crate::jobs::worker::JobHandler;
use crate::jobs::{self, JobContext, JobError};
use crate::nats::NatsState;
use crate::parsing::parse_release;
use crate::repositories::watch_directories as wd_repo;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use sqlx::PgPool;
use std::path::Path;
use time::OffsetDateTime;
use tokio_util::sync::CancellationToken;
use tracing::{info, trace, warn};
use uuid::Uuid;
use walkdir::WalkDir;

/// Supported file extensions by media type.
const BOOK_EXTENSIONS: &[&str] = &["epub", "kepub", "mobi", "azw", "azw3", "pdf", "cbz", "cbr"];
const AUDIO_EXTENSIONS: &[&str] = &["m4b", "m4a", "mp3", "flac", "ogg", "opus"];
const VIDEO_EXTENSIONS: &[&str] = &[
    "mkv", "mp4", "avi", "mov", "wmv", "flv", "webm", "m4v", "ts", "m2ts",
];

#[derive(Debug, Deserialize, Serialize)]
pub struct WatchDirectoryScanPayload {
    pub watch_directory_id: Uuid,
}

pub struct WatchDirectoryScanHandler {
    pool: PgPool,
    nats: NatsState,
}

impl WatchDirectoryScanHandler {
    pub fn new(pool: PgPool, nats: NatsState) -> Self {
        Self { pool, nats }
    }
}

#[async_trait]
impl JobHandler for WatchDirectoryScanHandler {
    fn job_type(&self) -> &'static str {
        "watch_directory_scan"
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
        nats_jobs::check_cancelled(&cancel)?;

        let payload: WatchDirectoryScanPayload = serde_json::from_value(payload)
            .map_err(|e| JobError::ExecutionFailed(format!("Invalid payload: {}", e)))?;

        let dir = wd_repo::get_by_id(&self.pool, payload.watch_directory_id)
            .await
            .map_err(|e| JobError::ExecutionFailed(e.to_string()))?
            .ok_or_else(|| JobError::ExecutionFailed("Watch directory not found".to_string()))?;

        // Mark as scanning
        let _ = wd_repo::set_scanning(&self.pool, dir.id).await;

        match self.scan_directory(&dir).await {
            Ok(stats) => {
                let _ = wd_repo::set_scan_complete(&self.pool, dir.id).await;

                // Enqueue notify job if we found new files
                if stats.discovered > 0 {
                    self.enqueue_notify(dir.id, &dir.name).await;
                }

                Ok(json!(stats))
            }
            Err(e) => {
                let error_msg = e.to_string();
                let _ = wd_repo::record_scan_error(&self.pool, dir.id, &error_msg).await;
                Err(e)
            }
        }
    }
}

#[derive(Debug, Serialize)]
struct ScanStats {
    discovered: usize,
    skipped_existing: usize,
    skipped_download: usize,
    errors: usize,
    total_files: usize,
}

impl WatchDirectoryScanHandler {
    async fn scan_directory(&self, dir: &wd_repo::WatchDirectory) -> Result<ScanStats, JobError> {
        let extensions: &[&str] = match dir.media_type.as_str() {
            "book" => BOOK_EXTENSIONS,
            "audiobook" => AUDIO_EXTENSIONS,
            "tv" | "movie" => VIDEO_EXTENSIONS,
            _ => BOOK_EXTENSIONS,
        };

        let media_kind = match dir.media_type.as_str() {
            "book" => "ebooks",
            "audiobook" => "audiobooks",
            "tv" => "tv",
            "movie" => "movies",
            _ => "ebooks",
        };

        let max_depth = if dir.recursive { usize::MAX } else { 1 };

        // Phase 1: Walk the filesystem on the blocking pool to collect file entries.
        let walk_path = dir.path.clone();
        let extensions_owned: Vec<String> = extensions.iter().map(|s| s.to_string()).collect();
        let files: Vec<(String, String)> = tokio::task::spawn_blocking(move || {
            let dir_path = Path::new(&walk_path);
            if !dir_path.exists() {
                return Err(JobError::ExecutionFailed(format!(
                    "Directory does not exist: {}",
                    walk_path
                )));
            }
            if !dir_path.is_dir() {
                return Err(JobError::ExecutionFailed(format!(
                    "Path is not a directory: {}",
                    walk_path
                )));
            }

            let walker = WalkDir::new(&walk_path)
                .max_depth(max_depth)
                .follow_links(true);

            let mut collected = Vec::new();
            for entry in walker.into_iter().filter_map(|e| e.ok()) {
                if !entry.file_type().is_file() {
                    continue;
                }
                let path = entry.path();
                let file_name = match path.file_name() {
                    Some(n) => n.to_string_lossy().to_string(),
                    None => continue,
                };
                if file_name.starts_with('.') {
                    continue;
                }
                let ext = path
                    .extension()
                    .map(|e| e.to_string_lossy().to_lowercase())
                    .unwrap_or_default();
                if !extensions_owned.iter().any(|e| e == &ext) {
                    continue;
                }
                collected.push((path.to_string_lossy().to_string(), file_name));
            }
            Ok(collected)
        })
        .await
        .map_err(|e| JobError::ExecutionFailed(format!("Join error: {e}")))??;

        // Phase 2: Process collected files in async context for DB operations.
        let mut stats = ScanStats {
            discovered: 0,
            skipped_existing: 0,
            skipped_download: 0,
            errors: 0,
            total_files: files.len(),
        };

        for (file_path_str, file_name) in &files {
            if let Err(e) = self
                .process_file(dir, file_path_str, file_name, media_kind, &mut stats)
                .await
            {
                warn!(error = %e, path = %file_path_str, "Error processing file");
                stats.errors += 1;
            }
        }

        info!(
            directory = %dir.name,
            total = stats.total_files,
            discovered = stats.discovered,
            skipped_existing = stats.skipped_existing,
            skipped_download = stats.skipped_download,
            errors = stats.errors,
            "Directory scan complete"
        );

        Ok(stats)
    }

    async fn process_file(
        &self,
        dir: &wd_repo::WatchDirectory,
        file_path: &str,
        file_name: &str,
        media_kind: &str,
        stats: &mut ScanStats,
    ) -> Result<(), JobError> {
        let fp = file_path.to_string();
        let metadata = tokio::task::spawn_blocking(move || std::fs::metadata(&fp))
            .await
            .map_err(|e| JobError::ExecutionFailed(format!("Join error: {e}")))?
            .map_err(|e| JobError::ExecutionFailed(format!("Cannot read file metadata: {}", e)))?;

        let file_size = metadata.len() as i64;
        let file_mtime = metadata
            .modified()
            .ok()
            .and_then(|t| OffsetDateTime::from(t).into());

        // Check if file already tracked with same mtime
        if let Ok(Some(existing)) = wd_repo::get_file_by_path(&self.pool, dir.id, file_path).await {
            let mtime_matches = match (existing.file_mtime, file_mtime) {
                (Some(a), Some(b)) => {
                    // 1-second tolerance for mtime comparison
                    (a - b).abs() < time::Duration::seconds(1)
                }
                (None, None) => true,
                _ => false,
            };

            if mtime_matches && existing.status != "error" {
                stats.skipped_existing += 1;
                return Ok(());
            }
            // Mtime changed or was in error - will re-process below
        }

        // Check if file matches an existing download (dedup)
        if let Ok(Some(download_id)) =
            wd_repo::file_exists_in_downloads(&self.pool, file_path).await
        {
            // File is tracked by a download - skip and record
            let input = wd_repo::WatchDirectoryFileInput {
                watch_directory_id: dir.id,
                file_path: file_path.to_string(),
                file_name: file_name.to_string(),
                file_size,
                file_hash: None,
                file_mtime,
                interpretation: None,
            };
            if let Ok(file) = wd_repo::upsert_file(&self.pool, input).await {
                let _ = wd_repo::mark_file_skipped(&self.pool, file.id, Some(download_id)).await;
            }
            stats.skipped_download += 1;
            return Ok(());
        }

        // Compute file hash
        let hash = compute_file_hash(file_path).await.ok();

        // Parse filename for metadata interpretation
        let stem = Path::new(file_name)
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| file_name.to_string());
        let parsed = parse_release(&stem, media_kind);
        let interpretation = serde_json::to_value(&parsed).ok();

        // Upsert the file as discovered
        let input = wd_repo::WatchDirectoryFileInput {
            watch_directory_id: dir.id,
            file_path: file_path.to_string(),
            file_name: file_name.to_string(),
            file_size,
            file_hash: hash,
            file_mtime,
            interpretation,
        };
        wd_repo::upsert_file(&self.pool, input)
            .await
            .map_err(|e| JobError::ExecutionFailed(e.to_string()))?;

        stats.discovered += 1;
        trace!(path = %file_path, title = %parsed.title, "Discovered file");

        Ok(())
    }

    async fn enqueue_notify(&self, dir_id: Uuid, dir_name: &str) {
        let _ = jobs::enqueue_optional(
            &self.pool,
            &self.nats.publisher,
            "watch_directory_notify",
            json!({ "watch_directory_id": dir_id }),
            JobContext::for_entity(
                "watch_directory",
                dir_id,
                format!("Sending callbacks for '{}'", dir_name),
            ),
        )
        .await;
    }
}

/// Compute SHA256 hash of a file (first 10MB for large files).
async fn compute_file_hash(path: &str) -> Result<String, std::io::Error> {
    use std::io::Read;

    let path = path.to_string();
    tokio::task::spawn_blocking(move || {
        const MAX_HASH_BYTES: usize = 10 * 1024 * 1024;

        let mut file = std::fs::File::open(&path)?;
        let metadata = file.metadata()?;
        let size = metadata.len() as usize;

        let mut hasher = Sha256::new();
        let read_size = size.min(MAX_HASH_BYTES);
        let mut buf = vec![0u8; read_size.min(65536)];
        let mut remaining = read_size;

        while remaining > 0 {
            let to_read = remaining.min(buf.len());
            let n = file.read(&mut buf[..to_read])?;
            if n == 0 {
                break;
            }
            hasher.update(&buf[..n]);
            remaining -= n;
        }

        Ok(const_hex::encode(hasher.finalize()))
    })
    .await
    .map_err(|e| std::io::Error::other(format!("Join error: {e}")))?
}
