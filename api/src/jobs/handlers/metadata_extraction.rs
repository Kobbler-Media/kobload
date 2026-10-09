//! Handler for metadata_extraction jobs.
//!
//! Enriches ScannedFile entries with computed metadata (scores, classifications,
//! file metadata from mediainfo, release interpretation) and persists the enriched
//! data to the database. Then builds and sends the HTTP callback.
//!
//! This replaces the inline metadata extraction that previously blocked the polling loop.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;
use sqlx::PgPool;
use tokio::sync::{Semaphore, broadcast};
use tracing::{info, warn};
use uuid::Uuid;

use crate::download_clients::ScannedFile;
use crate::file_scoring::{self, ScoringContext};
use crate::helpers::{broadcast_download_updated, log_event};
use crate::jobs::JobError;
use crate::jobs::worker::JobHandler;
use crate::nats::NatsState;
use crate::parsing::{FileRole, classify_file, extract_file_metadata, parse_release};
use crate::repositories::downloads as downloads_repo;
use nats_jobs::{DownloadCompletedEvent, DownloadFileInfo, FileGroupInfo, ReleaseInfo};
use tokio_util::sync::CancellationToken;

/// Maximum concurrent mediainfo subprocesses per job execution.
const MEDIAINFO_CONCURRENCY: usize = 5;

/// Handler for metadata extraction jobs.
pub struct MetadataExtractionHandler {
    pool: PgPool,
    nats: NatsState,
    http_client: reqwest::Client,
    download_events: broadcast::Sender<crate::types::DownloadStreamEvent>,
}

impl MetadataExtractionHandler {
    pub fn new(
        pool: PgPool,
        nats: NatsState,
        http_client: reqwest::Client,
        download_events: broadcast::Sender<crate::types::DownloadStreamEvent>,
    ) -> Self {
        Self {
            pool,
            nats,
            http_client,
            download_events,
        }
    }
}

#[async_trait]
impl JobHandler for MetadataExtractionHandler {
    fn job_type(&self) -> &'static str {
        "metadata_extraction"
    }

    fn max_concurrent(&self) -> usize {
        3
    }

    async fn execute(
        &self,
        job_id: Uuid,
        payload: Value,
        cancel: CancellationToken,
    ) -> Result<Value, JobError> {
        nats_jobs::check_cancelled(&cancel)?;

        let download_id: Uuid = payload
            .get("download_id")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| JobError::ExecutionFailed("missing download_id in payload".into()))?;

        info!(job_id = %job_id, download_id = %download_id, "Processing metadata extraction");

        // Load the download
        let download = downloads_repo::fetch_pending_download_by_id(&self.pool, download_id)
            .await
            .map_err(|e| JobError::ExecutionFailed(e.to_string()))?
            .ok_or_else(|| {
                JobError::ExecutionFailed(format!("download {download_id} not found"))
            })?;

        // Deserialize files_json
        let files: Vec<ScannedFile> = download
            .files_json
            .as_ref()
            .and_then(|json| serde_json::from_value(json.clone()).ok())
            .unwrap_or_default();

        if files.is_empty() {
            // No files to enrich - send callback if one was requested
            if download.has_callback() {
                send_callback_for_download(&self.http_client, &self.pool, &download, &[], None)
                    .await;
            } else {
                let _ = downloads_repo::mark_callback_sent(&self.pool, download_id).await;
            }
            broadcast_download_updated(&self.pool, &self.download_events, download_id).await;
            return Ok(serde_json::json!({ "status": "no_files" }));
        }

        // Wait for files to appear on disk — Transmission can report percentDone=1.0
        // while still moving files from incomplete to complete directory.
        wait_for_files_on_disk(&files, download_id).await;

        nats_jobs::check_cancelled(&cancel)?;

        // Score files using download metadata
        let scoring_ctx = build_scoring_context(&download);
        let scored = file_scoring::score_files(&files, &scoring_ctx);

        // Parse the release name
        let media_kind = download.media_kind.as_deref().unwrap_or("misc");
        let release_interpretation = parse_release(&download.name, media_kind);

        // Enrich each file: classify + extract metadata (async with bounded concurrency)
        let io_heavy_semaphore = Arc::new(Semaphore::new(MEDIAINFO_CONCURRENCY));
        let mut enrichment_handles = Vec::with_capacity(files.len());

        for file in &files {
            let sem = io_heavy_semaphore.clone();
            let file_path = file.absolute_path.clone();
            let file_clone = file.clone();
            let release_ref = release_interpretation.clone();
            let all_files = files.clone();
            let media_kind_owned = media_kind.to_string();

            enrichment_handles.push(tokio::spawn(async move {
                let _permit = sem.acquire().await.unwrap();

                let classification = classify_file(
                    &file_clone,
                    Some(&release_ref),
                    &all_files,
                    &media_kind_owned,
                );
                let file_metadata = extract_file_metadata(&file_path).await;
                (file_clone.id, classification, file_metadata)
            }));
        }

        // Collect enrichment results
        let mut enrichment_map = std::collections::HashMap::new();
        for handle in enrichment_handles {
            match handle.await {
                Ok((file_id, classification, file_metadata)) => {
                    enrichment_map.insert(file_id, (classification, file_metadata));
                }
                Err(e) => {
                    warn!(error = %e, "Enrichment task panicked");
                }
            }
        }

        // Build enriched files
        let mut enriched_files = files;
        for file in &mut enriched_files {
            // Find the scored entry for this file
            let scored_entry = scored.iter().find(|s| s.path == file.relative_path);
            file.score = scored_entry.map(|s| s.score);
            file.mime_type = scored_entry.and_then(|s| s.mime_type.clone());

            if let Some((classification, file_metadata)) = enrichment_map.remove(&file.id) {
                file.classification = Some(classification);
                file.file_metadata = Some(file_metadata);
            }
        }

        nats_jobs::check_cancelled(&cancel)?;

        // Persist enriched data
        let files_json = serde_json::to_value(&enriched_files)
            .map_err(|e| JobError::ExecutionFailed(format!("serialize files: {e}")))?;
        let release_json = serde_json::to_value(&release_interpretation).ok();

        downloads_repo::update_enriched_metadata(
            &self.pool,
            download_id,
            &files_json,
            release_json.as_ref(),
        )
        .await
        .map_err(|e| JobError::ExecutionFailed(e.to_string()))?;

        // Log enrichment completion before publishing/callbacks
        log_event(
            &self.pool,
            download_id,
            "metadata_extracted",
            &format!(
                "Metadata extraction complete: {} files enriched",
                enriched_files.len()
            ),
            None,
        )
        .await;

        // Check if the download actually contains valid content for its media kind.
        // Fake/premature releases (e.g. .exe bundles with no actual video) should be
        // failed here so Kobbler never sees them, and re-search can find a real release.
        if !has_valid_content(&enriched_files, media_kind) {
            let reason = format!(
                "No valid {media_kind} content files found (files: {})",
                enriched_files
                    .iter()
                    .map(|f| f.extension.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            warn!(download_id = %download_id, %reason, "Marking download as failed — no usable content");

            downloads_repo::mark_failed(&self.pool, download_id, &reason)
                .await
                .map_err(|e| JobError::ExecutionFailed(e.to_string()))?;

            log_event(
                &self.pool,
                download_id,
                "content_validation_failed",
                &reason,
                None,
            )
            .await;

            // Send failed callback so Kobbler knows, then trigger re-search
            if download.has_callback() {
                send_failed_callback_for_no_content(
                    &self.http_client,
                    &self.nats,
                    &self.pool,
                    &download,
                    &reason,
                )
                .await;
            }
            crate::polling::trigger_research_for_download_from_pool(
                &self.pool,
                &self.nats,
                download_id,
            )
            .await;

            broadcast_download_updated(&self.pool, &self.download_events, download_id).await;

            return Ok(serde_json::json!({
                "status": "failed_no_content",
                "reason": reason,
            }));
        }

        // Only notify external consumers if this download has a callback destination.
        // Downloads initiated from kobload's search UI have no callback_url.
        if download.has_callback() {
            // Try NATS ingest.ready first; fall back to HTTP callback
            let nats_sent = if self.nats.is_available() {
                crate::nats::publish_ingest_ready(
                    &self.nats,
                    &self.pool,
                    &download,
                    &enriched_files,
                    Some(&release_interpretation),
                )
                .await
            } else {
                false
            };

            if !nats_sent {
                // NATS unavailable or publish failed — fall back to HTTP callback
                send_callback_for_download(
                    &self.http_client,
                    &self.pool,
                    &download,
                    &enriched_files,
                    Some(&release_interpretation),
                )
                .await;
            }
        } else {
            // No callback destination — mark as sent so polling stops retrying
            let _ = downloads_repo::mark_callback_sent(&self.pool, download_id).await;
        }

        // Publish NATS event for UI updates (separate from ingest.ready)
        if self.nats.is_available() {
            let completed_event = DownloadCompletedEvent {
                download_id,
                status: download.status.clone(),
                name: download.name.clone(),
                metadata: download.metadata.clone(),
                files: enriched_files
                    .iter()
                    .map(|f| f.to_file_info(download_id))
                    .collect(),
                file_groups: None,
                release_interpretation: Some(release_interpretation.clone()),
                error: download.error.clone(),
                recoverable_error: download.files_scan_error.clone(),
                idempotency_key: None,
                is_upgrade: false,
                quality_score: None,
                replaces_download_id: None,
                release_info: download
                    .release_payload
                    .as_ref()
                    .and_then(release_info_from_payload),
            };
            crate::nats::publish_download_completed(&self.nats, &completed_event).await;
        }

        broadcast_download_updated(&self.pool, &self.download_events, download_id).await;

        Ok(serde_json::json!({
            "files_enriched": enriched_files.len(),
            "release_title": release_interpretation.title,
        }))
    }
}

/// Build scoring context from download metadata.
pub fn build_scoring_context(download: &downloads_repo::PendingDownload) -> ScoringContext {
    let title = if download.name.is_empty() {
        None
    } else {
        Some(download.name.clone())
    };

    let accepted_formats = download
        .metadata
        .get("media_want")
        .and_then(|mw| mw.get("format_preferences"))
        .and_then(|fp| fp.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_lowercase()))
                .collect()
        })
        .unwrap_or_default();

    ScoringContext {
        title,
        accepted_formats,
        media_kind: download.media_kind.clone(),
    }
}

/// Build a DownloadCompletedEvent from enriched ScannedFile data.
#[allow(clippy::too_many_arguments)]
pub fn build_callback_from_enriched(
    download_id: Uuid,
    name: &str,
    status: &str,
    metadata: &Value,
    files: &[ScannedFile],
    release_interpretation: Option<&crate::parsing::ParsedRelease>,
    error: Option<String>,
    recoverable_error: Option<String>,
    idempotency_key: Option<Uuid>,
    release_payload: Option<serde_json::Value>,
) -> DownloadCompletedEvent {
    // Re-score for ordering and grouping (lightweight, no I/O)
    let scored = files
        .iter()
        .map(|f| file_scoring::ScoredFile {
            path: f.relative_path.clone(),
            size: f.size,
            extension: f.extension.clone(),
            mime_type: f.mime_type.clone(),
            score: f.score.unwrap_or(0),
            group_key: file_scoring::normalize_for_grouping(
                &f.relative_path
                    .rsplit('/')
                    .next()
                    .unwrap_or(&f.relative_path)
                    .to_lowercase(),
            ),
        })
        .collect::<Vec<_>>();

    let groups = file_scoring::group_files(&scored);

    let mut file_infos: Vec<DownloadFileInfo> =
        files.iter().map(|f| f.to_file_info(download_id)).collect();

    // Backfill matched_episode from release_interpretation for obfuscated single-video-file downloads.
    // When a filename is gibberish (e.g. "lKpE5HBn7S0zOLkyd95Y3gb3zfAIoYYF.mkv"), per-file
    // classification can't determine the episode. But if the release title parses to a specific
    // episode and there's exactly one main-content video file, we can safely assign it.
    if let Some(interp) = release_interpretation
        && let Some(tv) = interp.media.as_tv()
        && !tv.episodes.is_empty()
    {
        let main_video_count = file_infos
            .iter()
            .filter(|f| {
                is_video_ext(&f.extension)
                    && f.classification
                        .as_ref()
                        .is_some_and(|c| c.role == FileRole::MainContent)
            })
            .count();
        if main_video_count == 1 {
            for fi in &mut file_infos {
                if fi.matched_episode.is_none()
                    && is_video_ext(&fi.extension)
                    && fi
                        .classification
                        .as_ref()
                        .is_some_and(|c| c.role == FileRole::MainContent)
                {
                    fi.matched_episode = Some(tv.episodes[0].to_string());
                    tracing::info!(
                        episode = tv.episodes[0],
                        path = %fi.path,
                        "Backfilled matched_episode from release interpretation (obfuscated filename)"
                    );
                }
            }
        }
    }

    let file_groups = if groups.is_empty() {
        None
    } else {
        Some(
            groups
                .into_iter()
                .map(|g| FileGroupInfo {
                    name: g.name,
                    formats: g.formats,
                    best_path: g.best_path,
                })
                .collect(),
        )
    };

    let release_info = release_payload.as_ref().and_then(release_info_from_payload);

    DownloadCompletedEvent {
        download_id,
        status: status.to_string(),
        name: name.to_string(),
        metadata: metadata.clone(),
        files: file_infos,
        file_groups,
        release_interpretation: release_interpretation.cloned(),
        error,
        recoverable_error,
        idempotency_key,
        is_upgrade: false,
        quality_score: None,
        replaces_download_id: None,
        release_info,
    }
}

/// Extract structured `ReleaseInfo` from the untyped `release_payload` JSON.
pub fn release_info_from_payload(payload: &serde_json::Value) -> Option<ReleaseInfo> {
    let title = payload.get("title")?.as_str()?.to_string();
    Some(ReleaseInfo {
        title,
        guid: payload
            .get("guid")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        indexer: payload
            .get("indexer")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        matched_target: payload
            .get("matched_target")
            .and_then(|v| serde_json::from_value(v.clone()).ok()),
        quality_score: payload.get("quality_score").and_then(|v| v.as_f64()),
        total_score: payload.get("total_score").and_then(|v| v.as_f64()),
        size: payload.get("size").and_then(|v| v.as_i64()),
        protocol: payload
            .get("protocol")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
        info_hash: payload
            .get("info_hash")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()),
    })
}

/// Send HTTP callback for a download using enriched file data.
async fn send_callback_for_download(
    http_client: &reqwest::Client,
    pool: &PgPool,
    download: &downloads_repo::PendingDownload,
    files: &[ScannedFile],
    release_interpretation: Option<&crate::parsing::ParsedRelease>,
) {
    // Get or create idempotency key
    let idempotency_key =
        match downloads_repo::get_or_create_idempotency_key(pool, download.id).await {
            Ok(key) => Some(key),
            Err(e) => {
                warn!(
                    download_id = %download.id,
                    error = %e,
                    "Failed to get/create idempotency key"
                );
                None
            }
        };

    let payload = build_callback_from_enriched(
        download.id,
        &download.name,
        &download.status,
        &download.metadata,
        files,
        release_interpretation,
        download.error.clone(),
        download.files_scan_error.clone(),
        idempotency_key,
        download.release_payload.clone(),
    );

    let payload_json = serde_json::to_value(&payload).ok();

    let mut request = http_client.post(&download.callback_url).json(&payload);
    if let Some(token) = &download.auth_token {
        request = request.header("Authorization", format!("Bearer {}", token));
    }
    if let Some(key) = idempotency_key {
        request = request.header("X-Idempotency-Key", key.to_string());
    }

    let new_attempts = download.callback_attempts + 1;
    let _ = downloads_repo::update_callback_attempt(pool, download.id, new_attempts).await;

    match request.send().await {
        Ok(resp) if resp.status().is_success() => {
            let _ = downloads_repo::mark_callback_sent(pool, download.id).await;
            log_event(
                pool,
                download.id,
                "callback_sent",
                "Callback succeeded (via metadata extraction job)",
                Some(serde_json::json!({
                    "url": download.callback_url,
                    "status": resp.status().as_u16(),
                    "attempt": new_attempts,
                    "payload": payload_json,
                })),
            )
            .await;
        }
        Ok(resp) => {
            let status = resp.status();
            let body = resp.text().await.ok();
            let error_msg = format!(
                "HTTP {}: {}",
                status.as_u16(),
                body.as_deref().unwrap_or("")
            );
            warn!(status = %status, "Callback failed");
            let _ = downloads_repo::update_callback_error(pool, download.id, &error_msg).await;
            log_event(
                pool,
                download.id,
                "callback_failed",
                &format!("Callback failed (attempt {})", new_attempts),
                Some(serde_json::json!({
                    "url": download.callback_url,
                    "status": status.as_u16(),
                    "response_body": body,
                    "attempt": new_attempts,
                    "payload": payload_json,
                })),
            )
            .await;
        }
        Err(err) => {
            let error_msg = format!("Request error: {}", err);
            warn!(error = %err, "Callback request failed");
            let _ = downloads_repo::update_callback_error(pool, download.id, &error_msg).await;
            log_event(
                pool,
                download.id,
                "callback_failed",
                &format!("Callback request failed (attempt {})", new_attempts),
                Some(serde_json::json!({
                    "url": download.callback_url,
                    "error": err.to_string(),
                    "attempt": new_attempts,
                    "payload": payload_json,
                })),
            )
            .await;
        }
    }
}

/// Known video extensions for content validation.
const VIDEO_EXTENSIONS: &[&str] = &[
    "mkv", "mp4", "avi", "wmv", "webm", "m4v", "flv", "mov", "mpg", "mpeg", "ts", "m2ts", "vob",
];

fn is_video_ext(ext: &str) -> bool {
    VIDEO_EXTENSIONS.contains(&ext.to_lowercase().as_str())
}

/// Known audio extensions for content validation.
const AUDIO_EXTENSIONS: &[&str] = &[
    "mp3", "flac", "m4a", "m4b", "ogg", "opus", "wav", "wma", "aac", "alac", "ape", "dsf", "dff",
];

/// Known ebook extensions for content validation.
const EBOOK_EXTENSIONS: &[&str] = &[
    "epub", "mobi", "azw", "azw3", "pdf", "cbz", "cbr", "cb7", "djvu", "fb2", "lit", "pdb",
];

/// Check whether the enriched files contain at least one file that is valid
/// content for the given media kind. Fake/premature releases typically contain
/// .exe, .txt, .url, .html etc. but no actual video/audio/ebook files.
fn has_valid_content(files: &[ScannedFile], media_kind: &str) -> bool {
    let dominated_extensions: &[&str] = match media_kind {
        "movie" | "tv" => VIDEO_EXTENSIONS,
        "audiobook" => AUDIO_EXTENSIONS,
        "book" | "ebook" => EBOOK_EXTENSIONS,
        _ => return true, // Unknown media kind — can't validate, allow through
    };

    files.iter().any(|f| {
        let ext = f.extension.to_lowercase();
        let is_valid_ext = dominated_extensions.contains(&ext.as_str());
        let is_main_content = f
            .classification
            .as_ref()
            .is_some_and(|c| c.role == FileRole::MainContent);
        // Accept if the file has the right extension AND is classified as main content,
        // OR just has the right extension if classification didn't run for some reason.
        is_valid_ext && (is_main_content || f.classification.is_none())
    })
}

/// Send a failed callback (via NATS or HTTP) when content validation fails.
async fn send_failed_callback_for_no_content(
    http_client: &reqwest::Client,
    nats: &NatsState,
    pool: &PgPool,
    download: &downloads_repo::PendingDownload,
    reason: &str,
) {
    let idempotency_key = downloads_repo::get_or_create_idempotency_key(pool, download.id)
        .await
        .ok();

    let payload = build_callback_from_enriched(
        download.id,
        &download.name,
        "failed",
        &download.metadata,
        &[],
        None,
        Some(reason.to_string()),
        None,
        idempotency_key,
        download.release_payload.clone(),
    );

    // Try NATS first
    if nats.is_available()
        && let Ok(payload_json) = serde_json::to_value(&payload)
        && nats
            .publisher()
            .publish_event("ingest.ready", payload_json)
            .await
            .is_some()
    {
        let _ = downloads_repo::mark_callback_sent(pool, download.id).await;
        return;
    }

    // Fall back to HTTP
    let mut request = http_client.post(&download.callback_url).json(&payload);
    if let Some(token) = &download.auth_token {
        request = request.header("Authorization", format!("Bearer {}", token));
    }
    if let Some(key) = idempotency_key {
        request = request.header("X-Idempotency-Key", key.to_string());
    }
    if let Ok(resp) = request.send().await
        && resp.status().is_success()
    {
        let _ = downloads_repo::mark_callback_sent(pool, download.id).await;
    }
}

async fn wait_for_files_on_disk(files: &[ScannedFile], download_id: Uuid) {
    use crate::archive::is_archive_nested;

    // Find the largest file that should exist on disk (not inside an archive)
    let largest_file = files
        .iter()
        .filter(|f| !is_archive_nested(&f.absolute_path))
        .max_by_key(|f| f.size);

    let Some(target) = largest_file else {
        return;
    };

    // Quick check — file already exists
    if tokio::fs::metadata(&target.absolute_path).await.is_ok() {
        return;
    }

    tracing::info!(
        download_id = %download_id,
        path = %target.absolute_path,
        "Waiting for file to appear on disk (download client may still be moving files)"
    );

    // Retry with increasing delays: 1s, 2s, 4s, 8s, 15s (total ~30s max wait)
    let delays = [1, 2, 4, 8, 15];
    for delay in delays {
        tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
        if tokio::fs::metadata(&target.absolute_path).await.is_ok() {
            tracing::info!(
                download_id = %download_id,
                path = %target.absolute_path,
                waited_secs = delay,
                "File appeared on disk"
            );
            return;
        }
    }

    tracing::warn!(
        download_id = %download_id,
        path = %target.absolute_path,
        "File still not found after waiting ~30s, proceeding anyway"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parsing::{FileClassification, FileRole};

    fn make_enriched_file(name: &str, score: i32) -> ScannedFile {
        ScannedFile {
            id: Uuid::now_v7(),
            absolute_path: format!("/dl/{name}"),
            relative_path: name.to_string(),
            size: 1_000_000,
            extension: name.rsplit('.').next().unwrap_or("").to_string(),
            split_rar_volumes: None,
            distributed_rar_sources: None,
            distributed_numeric_sources: None,
            numeric_split_type: None,
            score: Some(score),
            classification: Some(FileClassification {
                role: FileRole::MainContent,
                parsed: None,
                confidence: 0.9,
                group_key: Some(file_scoring::normalize_for_grouping(
                    &name.rsplit('/').next().unwrap_or(name).to_lowercase(),
                )),
            }),
            file_metadata: None,
            mime_type: None,
        }
    }

    #[test]
    fn build_callback_includes_all_enrichment() {
        let download_id = Uuid::now_v7();
        let files = vec![
            make_enriched_file("book.epub", 100),
            make_enriched_file("book.mobi", 80),
        ];
        let release = crate::parsing::parse_release("MyBook-2024-EPUB-GROUP", "ebooks");

        let payload = build_callback_from_enriched(
            download_id,
            "MyBook-2024-EPUB-GROUP",
            "completed",
            &serde_json::json!({}),
            &files,
            Some(&release),
            None,
            None,
            None,
            None,
        );

        assert_eq!(payload.download_id, download_id);
        assert_eq!(payload.status, "completed");
        assert_eq!(payload.files.len(), 2);
        assert!(payload.release_interpretation.is_some());
        // Files are nats_jobs::DownloadFileInfo now
        assert!(payload.files[0].score.is_some());
        assert!(payload.files[1].score.is_some());
    }

    #[test]
    fn build_callback_handles_empty_files() {
        let download_id = Uuid::now_v7();
        let payload = build_callback_from_enriched(
            download_id,
            "test-download",
            "failed",
            &serde_json::json!({"key": "val"}),
            &[],
            None,
            Some("download failed".to_string()),
            None,
            None,
            None,
        );

        assert_eq!(payload.status, "failed");
        assert!(payload.files.is_empty());
        assert!(payload.file_groups.is_none());
        assert_eq!(payload.error, Some("download failed".to_string()));
        assert!(payload.release_interpretation.is_none());
    }

    #[test]
    fn build_callback_preserves_idempotency_key() {
        let download_id = Uuid::now_v7();
        let key = Uuid::now_v7();
        let payload = build_callback_from_enriched(
            download_id,
            "test-download",
            "completed",
            &serde_json::json!({}),
            &[],
            None,
            None,
            None,
            Some(key),
            None,
        );
        assert_eq!(payload.idempotency_key, Some(key));
    }

    fn make_file_with_role(name: &str, role: FileRole) -> ScannedFile {
        ScannedFile {
            id: Uuid::now_v7(),
            absolute_path: format!("/dl/{name}"),
            relative_path: name.to_string(),
            size: 1_000_000,
            extension: name.rsplit('.').next().unwrap_or("").to_string(),
            split_rar_volumes: None,
            distributed_rar_sources: None,
            distributed_numeric_sources: None,
            numeric_split_type: None,
            score: Some(0),
            classification: Some(FileClassification {
                role,
                parsed: None,
                confidence: 0.9,
                group_key: None,
            }),
            file_metadata: None,
            mime_type: None,
        }
    }

    #[test]
    fn valid_content_movie_with_video() {
        let files = vec![
            make_file_with_role("movie.mkv", FileRole::MainContent),
            make_file_with_role("info.nfo", FileRole::Metadata),
        ];
        assert!(has_valid_content(&files, "movie"));
    }

    #[test]
    fn no_valid_content_movie_exe_only() {
        let files = vec![
            make_file_with_role("setup.exe", FileRole::Unknown),
            make_file_with_role("readme.txt", FileRole::Metadata),
            make_file_with_role("movie.url", FileRole::Unknown),
        ];
        assert!(!has_valid_content(&files, "movie"));
    }

    #[test]
    fn no_valid_content_movie_sample_video() {
        // A video file classified as Sample, not MainContent — still not valid
        let files = vec![
            make_file_with_role("sample.mkv", FileRole::Sample),
            make_file_with_role("info.nfo", FileRole::Metadata),
        ];
        assert!(!has_valid_content(&files, "movie"));
    }

    #[test]
    fn valid_content_tv_with_video() {
        let files = vec![make_file_with_role("episode.mp4", FileRole::MainContent)];
        assert!(has_valid_content(&files, "tv"));
    }

    #[test]
    fn valid_content_book_with_epub() {
        let files = vec![make_file_with_role("book.epub", FileRole::MainContent)];
        assert!(has_valid_content(&files, "book"));
    }

    #[test]
    fn valid_content_audiobook_with_m4b() {
        let files = vec![make_file_with_role("book.m4b", FileRole::MainContent)];
        assert!(has_valid_content(&files, "audiobook"));
    }

    #[test]
    fn valid_content_unknown_media_kind_always_passes() {
        let files = vec![make_file_with_role("random.exe", FileRole::Unknown)];
        assert!(has_valid_content(&files, "misc"));
    }

    #[test]
    fn no_valid_content_empty_files() {
        assert!(!has_valid_content(&[], "movie"));
    }

    #[test]
    fn backfill_matched_episode_for_obfuscated_single_video() {
        let download_id = Uuid::now_v7();
        let files = vec![make_file_with_role(
            "lKpE5HBn7S0zOLkyd95Y3gb3zfAIoYYF.mkv",
            FileRole::MainContent,
        )];
        // Release title parses as TV S03E08
        let release = crate::parsing::parse_release(
            "Shrinking.S03E08.Depression.Diet.1080p.ATVP.WEB-DL.DDP5.1.Atmos.H.264-FLUX",
            "tv",
        );

        let payload = build_callback_from_enriched(
            download_id,
            "Shrinking.S03E08.Depression.Diet.1080p.ATVP.WEB-DL.DDP5.1.Atmos.H.264-FLUX",
            "completed",
            &serde_json::json!({}),
            &files,
            Some(&release),
            None,
            None,
            None,
            None,
        );

        assert_eq!(payload.files.len(), 1);
        assert_eq!(
            payload.files[0].matched_episode,
            Some("8".to_string()),
            "Should backfill episode from release interpretation"
        );
    }

    #[test]
    fn no_backfill_when_file_already_has_matched_episode() {
        let download_id = Uuid::now_v7();
        // File whose classification already parsed the episode
        let mut file = make_enriched_file("Show.S01E05.mkv", 100);
        file.classification = Some(FileClassification {
            role: FileRole::MainContent,
            parsed: Some(nats_jobs::ParsedMedia::Tv(nats_jobs::TvParsed {
                show_name: "Show".to_string(),
                season: Some(1),
                episodes: vec![5],
                episode_title: None,
                air_date: None,
                absolute_episode: None,
                is_pack: false,
                is_daily: false,
                part: None,
                is_special: false,
                special_type: None,
                year: None,
            })),
            confidence: 0.9,
            group_key: None,
        });

        let release = crate::parsing::parse_release("Show.S01E05.1080p.WEB-DL.H.264-GROUP", "tv");

        let payload = build_callback_from_enriched(
            download_id,
            "Show.S01E05.1080p.WEB-DL.H.264-GROUP",
            "completed",
            &serde_json::json!({}),
            &[file],
            Some(&release),
            None,
            None,
            None,
            None,
        );

        // matched_episode comes from to_file_info, not backfill
        assert_eq!(payload.files[0].matched_episode, Some("5".to_string()));
    }

    #[test]
    fn no_backfill_when_sample_present_with_main() {
        let download_id = Uuid::now_v7();
        let files = vec![
            make_file_with_role("obfuscated.mkv", FileRole::MainContent),
            make_file_with_role("Sample/sample.mkv", FileRole::Sample),
        ];
        let release =
            crate::parsing::parse_release("Shrinking.S03E08.1080p.WEB-DL.H.264-GROUP", "tv");

        let payload = build_callback_from_enriched(
            download_id,
            "Shrinking.S03E08.1080p.WEB-DL.H.264-GROUP",
            "completed",
            &serde_json::json!({}),
            &files,
            Some(&release),
            None,
            None,
            None,
            None,
        );

        // Only 1 MainContent video → backfill should trigger for the main file
        assert_eq!(
            payload.files[0].matched_episode,
            Some("8".to_string()),
            "Main content file should get backfilled episode"
        );
        assert_eq!(
            payload.files[1].matched_episode, None,
            "Sample file should NOT get backfilled episode"
        );
    }

    #[test]
    fn no_backfill_for_multi_episode_main_videos() {
        let download_id = Uuid::now_v7();
        let files = vec![
            make_file_with_role("ep1.mkv", FileRole::MainContent),
            make_file_with_role("ep2.mkv", FileRole::MainContent),
        ];
        let release = crate::parsing::parse_release("Show.S01E01.1080p.WEB-DL.H.264-GROUP", "tv");

        let payload = build_callback_from_enriched(
            download_id,
            "Show.S01E01.1080p.WEB-DL.H.264-GROUP",
            "completed",
            &serde_json::json!({}),
            &files,
            Some(&release),
            None,
            None,
            None,
            None,
        );

        // 2 MainContent videos → unsafe to assign, both should be None
        assert_eq!(payload.files[0].matched_episode, None);
        assert_eq!(payload.files[1].matched_episode, None);
    }
}
