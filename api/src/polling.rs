use std::time::Duration;
use tokio::time::sleep;
use tracing::{error, info, warn};

use uuid::Uuid;

use crate::download_clients::{
    DownloadClientError, PollResult, ScannedFile, expand_archives, select_download_client,
};
use crate::helpers::{broadcast_download_updated, log_event};
use crate::jobs;
use crate::jobs::handlers::build_callback_from_enriched;
use crate::nats;
use crate::parsing::ParsedRelease;
use crate::repositories::downloads as downloads_repo;
use crate::repositories::watch as watch_repo;
use crate::state::AppState;

pub use crate::repositories::downloads::PendingDownload;

/// Result of attempting to retry a files scan for a completed download
enum FilesScanRetryResult {
    /// Files are now accessible, here's the poll result with files
    Recovered(PollResult),
    /// Files still inaccessible, here's the error message
    StillFailing(String),
    /// Could not poll (missing client_id, external_id, or client selection failed)
    CouldNotPoll,
}

/// Attempt to re-poll a download client to scan files for a completed download
/// that previously had a files_scan_error
async fn retry_files_scan(state: &AppState, download: &PendingDownload) -> FilesScanRetryResult {
    let (Some(client_id), Some(external_id)) =
        (download.download_client_id, download.external_id.as_deref())
    else {
        return FilesScanRetryResult::CouldNotPoll;
    };

    let Ok(client) = select_download_client(&state.pool, Some(client_id), &download.protocol).await
    else {
        return FilesScanRetryResult::CouldNotPoll;
    };

    let handler = match state.download_clients.get(client.client_type.as_str()) {
        Some(handler) => handler,
        None => return FilesScanRetryResult::CouldNotPoll,
    };

    let poll_result = handler.poll(&state.http_client, &client, external_id).await;

    let Ok(result) = poll_result else {
        return FilesScanRetryResult::CouldNotPoll;
    };

    if result.files_scan_error.is_none() && !result.files.is_empty() {
        // Files are now accessible - expand archives
        let expanded_files = expand_archives(
            result.files.clone(),
            state.unrar_path.as_deref(),
            state.sevenz_path.as_deref(),
        )
        .await;
        FilesScanRetryResult::Recovered(PollResult::completed(expanded_files))
    } else {
        // Still can't scan files
        let error_msg = result
            .files_scan_error
            .unwrap_or_else(|| "files still inaccessible".to_string());
        FilesScanRetryResult::StillFailing(error_msg)
    }
}

/// Trigger metadata extraction for a completed download.
///
/// If NATS is available: enqueue a metadata_extraction job (with dedup).
/// If NATS is unavailable: fall back to inline callback without enrichment.
async fn trigger_metadata_extraction(state: &AppState, download: &PendingDownload) {
    // Try to enqueue via NATS
    if state.nats.is_available() {
        // Dedup: don't enqueue if there's already an active job for this download
        let already_active = jobs::has_active_job(
            &state.pool,
            "metadata_extraction",
            "download_id",
            &download.id.to_string(),
        )
        .await
        .unwrap_or(false);

        if !already_active {
            let payload = serde_json::json!({ "download_id": download.id.to_string() });
            let context = jobs::JobContext {
                entity_type: Some("download".to_string()),
                entity_id: Some(download.id),
                description: format!("Metadata extraction for {}", download.name),
                display: None,
            };

            if let Some(job_id) = jobs::enqueue_optional(
                &state.pool,
                state.nats.publisher(),
                "metadata_extraction",
                payload,
                context,
            )
            .await
            {
                info!(
                    download_id = %download.id,
                    job_id = %job_id,
                    "Metadata extraction job enqueued"
                );
                return;
            }
        } else {
            info!(
                download_id = %download.id,
                "Metadata extraction job already active, skipping"
            );
            return;
        }
    }

    // Fallback: NATS unavailable or enqueue failed - send callback without enrichment
    warn!(
        download_id = %download.id,
        "NATS unavailable, sending callback without metadata enrichment"
    );
    if download.has_callback() {
        send_callback_inline(state, download).await;
    } else {
        let _ = downloads_repo::mark_callback_sent(&state.pool, download.id).await;
    }
}

/// Send callback using enriched data already stored in the download's files_json.
/// Used for retry paths where enrichment has already been done.
/// Tries NATS first, falls back to HTTP.
async fn send_callback_from_enriched(state: &AppState, download: &PendingDownload) -> bool {
    if !download.has_callback() {
        let _ = downloads_repo::mark_callback_sent(&state.pool, download.id).await;
        return true;
    }

    let files: Vec<ScannedFile> = download
        .files_json
        .as_ref()
        .and_then(|json| serde_json::from_value(json.clone()).ok())
        .unwrap_or_default();

    // Deserialize stored release interpretation
    let release_interpretation: Option<ParsedRelease> =
        downloads_repo::get_reingest_info(&state.pool, download.id)
            .await
            .ok()
            .flatten()
            .and_then(|info| {
                info.release_interpretation_json
                    .and_then(|json| serde_json::from_value(json).ok())
            });

    // Try NATS first
    if nats::publish_ingest_ready(
        &state.nats,
        &state.pool,
        download,
        &files,
        release_interpretation.as_ref(),
    )
    .await
    {
        return true;
    }

    // Fall back to HTTP callback
    let idempotency_key =
        match downloads_repo::get_or_create_idempotency_key(&state.pool, download.id).await {
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
        &files,
        release_interpretation.as_ref(),
        download.error.clone(),
        download.files_scan_error.clone(),
        idempotency_key,
        download.release_payload.clone(),
    );

    send_callback_payload(state, download, &payload, idempotency_key).await
}

/// Send a pre-built callback payload via HTTP.
async fn send_callback_payload(
    state: &AppState,
    download: &PendingDownload,
    payload: &nats_jobs::DownloadCompletedEvent,
    idempotency_key: Option<Uuid>,
) -> bool {
    let payload_json = serde_json::to_value(payload).ok();

    let mut request = state.http_client.post(&download.callback_url).json(payload);
    if let Some(token) = &download.auth_token {
        request = request.header("Authorization", format!("Bearer {}", token));
    }
    if let Some(key) = idempotency_key {
        request = request.header("X-Idempotency-Key", key.to_string());
    }

    let new_attempts = download.callback_attempts + 1;
    let _ = downloads_repo::update_callback_attempt(&state.pool, download.id, new_attempts).await;

    match request.send().await {
        Ok(resp) if resp.status().is_success() => {
            let _ = downloads_repo::mark_callback_sent(&state.pool, download.id).await;
            log_event(
                &state.pool,
                download.id,
                "callback_sent",
                "Callback succeeded",
                Some(serde_json::json!({
                    "url": download.callback_url,
                    "status": resp.status().as_u16(),
                    "attempt": new_attempts,
                    "payload": payload_json,
                })),
            )
            .await;
            true
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
            let _ =
                downloads_repo::update_callback_error(&state.pool, download.id, &error_msg).await;
            log_event(
                &state.pool,
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
            false
        }
        Err(err) => {
            let error_msg = format!("Request error: {}", err);
            warn!(error = %err, "Callback request failed");
            let _ =
                downloads_repo::update_callback_error(&state.pool, download.id, &error_msg).await;
            log_event(
                &state.pool,
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
            false
        }
    }
}

/// Inline callback without enrichment (fallback when NATS unavailable).
/// Sends files with basic info only (no scores, classification, or file metadata).
async fn send_callback_inline(state: &AppState, download: &PendingDownload) {
    let files: Vec<ScannedFile> = download
        .files_json
        .as_ref()
        .and_then(|json| serde_json::from_value(json.clone()).ok())
        .unwrap_or_default();

    let idempotency_key =
        (downloads_repo::get_or_create_idempotency_key(&state.pool, download.id).await).ok();

    let payload = build_callback_from_enriched(
        download.id,
        &download.name,
        &download.status,
        &download.metadata,
        &files,
        None,
        download.error.clone(),
        download.files_scan_error.clone(),
        idempotency_key,
        download.release_payload.clone(),
    );

    send_callback_payload(state, download, &payload, idempotency_key).await;
}

/// Send callback for a failed download (no files to enrich, just error info).
async fn send_failed_callback(state: &AppState, download: &PendingDownload) {
    // Publish NATS event for UI (fires for all downloads, including no-callback ones)
    if state.nats.is_available() {
        let event = nats_jobs::DownloadCompletedEvent {
            download_id: download.id,
            status: download.status.clone(),
            name: download.name.clone(),
            metadata: download.metadata.clone(),
            files: vec![],
            file_groups: None,
            release_interpretation: None,
            error: download.error.clone(),
            recoverable_error: None,
            idempotency_key: None,
            is_upgrade: false,
            quality_score: None,
            replaces_download_id: None,
            release_info: download
                .release_payload
                .as_ref()
                .and_then(crate::jobs::handlers::release_info_from_payload),
        };
        nats::publish_download_completed(&state.nats, &event).await;
    }

    if !download.has_callback() {
        let _ = downloads_repo::mark_callback_sent(&state.pool, download.id).await;
        return;
    }

    send_callback_inline(state, download).await;
}

pub async fn poll_downloads(state: AppState) {
    loop {
        let pending = downloads_repo::fetch_pending_downloads(&state.pool)
            .await
            .unwrap_or_else(|e| {
                error!(error = %e, "failed to fetch pending downloads");
                Vec::new()
            });

        for download in pending {
            if download.status == "completed" || download.status == "failed" {
                // Handle files_scan_error retry - re-poll to check if files are now accessible
                if download.files_scan_error.is_some() && download.status == "completed" {
                    // Try to re-poll download client to scan files
                    let retry_result = retry_files_scan(&state, &download).await;
                    match retry_result {
                        FilesScanRetryResult::Recovered(expanded_result) => {
                            // Files are now accessible - clear error and enqueue metadata extraction
                            let files_json = if !expanded_result.files.is_empty() {
                                serde_json::to_value(&expanded_result.files).ok()
                            } else {
                                None
                            };
                            let _ = downloads_repo::clear_files_scan_error(
                                &state.pool,
                                download.id,
                                expanded_result.file_path.as_deref(),
                                files_json.as_ref(),
                            )
                            .await;

                            log_event(
                                &state.pool,
                                download.id,
                                "files_scan_recovered",
                                "Files are now accessible after retry",
                                None,
                            )
                            .await;

                            trigger_metadata_extraction(&state, &download).await;
                            broadcast_download_updated(
                                &state.pool,
                                &state.download_events,
                                download.id,
                            )
                            .await;
                        }
                        FilesScanRetryResult::StillFailing(error_msg) => {
                            // Still can't scan files - update last attempt time for backoff
                            let _ = downloads_repo::update_files_scan_error(
                                &state.pool,
                                download.id,
                                &error_msg,
                            )
                            .await;
                            broadcast_download_updated(
                                &state.pool,
                                &state.download_events,
                                download.id,
                            )
                            .await;
                        }
                        FilesScanRetryResult::CouldNotPoll => {
                            // If we couldn't re-poll, update last attempt time for backoff
                            let _ =
                                downloads_repo::touch_callback_attempt(&state.pool, download.id)
                                    .await;
                        }
                    }
                    continue;
                }

                if download.callback_sent_at.is_none() {
                    if download.status == "completed" {
                        // Restore files from stored JSON
                        let files: Vec<ScannedFile> = download
                            .files_json
                            .as_ref()
                            .and_then(|json| serde_json::from_value(json.clone()).ok())
                            .unwrap_or_default();

                        if files.first().is_some_and(|f| f.is_enriched()) {
                            // Files are already enriched - send callback from stored data
                            let _ = send_callback_from_enriched(&state, &download).await;
                        } else {
                            // Not enriched yet - trigger metadata extraction
                            trigger_metadata_extraction(&state, &download).await;
                        }
                    } else {
                        // Failed downloads don't need enrichment
                        send_failed_callback(&state, &download).await;
                    }
                }
                continue;
            }

            let Some(client_id) = download.download_client_id else {
                continue;
            };
            let Some(external_id) = download.external_id.as_deref() else {
                continue;
            };

            let client = match select_download_client(
                &state.pool,
                Some(client_id),
                &download.protocol,
            )
            .await
            {
                Ok(client) => client,
                Err(_) => continue,
            };

            let handler = match state.download_clients.get(client.client_type.as_str()) {
                Some(handler) => handler,
                None => {
                    let err = DownloadClientError::UnsupportedClient;
                    let _ = downloads_repo::mark_failed(&state.pool, download.id, &err.to_string())
                        .await;
                    log_event(
                        &state.pool,
                        download.id,
                        "poll_error",
                        "Polling download client failed",
                        Some(serde_json::json!({ "error": err.to_string() })),
                    )
                    .await;
                    broadcast_download_updated(&state.pool, &state.download_events, download.id)
                        .await;
                    continue;
                }
            };

            let poll_result = handler.poll(&state.http_client, &client, external_id).await;

            let poll_result = match poll_result {
                Ok(result) => result,
                Err(err) => {
                    let is_connection_error = matches!(&err, DownloadClientError::Request(_));
                    if is_connection_error {
                        let _ = downloads_repo::update_poll_status(
                            &state.pool,
                            download.id,
                            Some(&err.to_string()),
                            None,
                        )
                        .await;
                        log_event(
                            &state.pool,
                            download.id,
                            "poll_error",
                            "Polling download client failed (will retry)",
                            Some(serde_json::json!({ "error": err.to_string(), "recoverable": true })),
                        )
                        .await;
                    } else {
                        let _ =
                            downloads_repo::mark_failed(&state.pool, download.id, &err.to_string())
                                .await;
                        log_event(
                            &state.pool,
                            download.id,
                            "poll_error",
                            "Polling download client failed",
                            Some(serde_json::json!({ "error": err.to_string() })),
                        )
                        .await;
                    }
                    broadcast_download_updated(&state.pool, &state.download_events, download.id)
                        .await;
                    continue;
                }
            };

            let _ = downloads_repo::update_poll_status(
                &state.pool,
                download.id,
                poll_result.error.as_deref(),
                poll_result.progress,
            )
            .await;

            // Broadcast progress update
            broadcast_download_updated(&state.pool, &state.download_events, download.id).await;

            if poll_result.status == download.status {
                continue;
            }

            let status_update = downloads_repo::DownloadStatusUpdate {
                status: &poll_result.status,
                file_path: poll_result.file_path.as_deref(),
                error: poll_result.error.as_deref(),
                progress: poll_result.progress,
                files_scan_error: poll_result.files_scan_error.as_deref(),
                client_data: poll_result.client_data.as_ref(),
            };
            let _ = downloads_repo::update_status(&state.pool, download.id, &status_update).await;

            // Log status change
            log_event(
                &state.pool,
                download.id,
                "status_changed",
                &format!(
                    "Status changed from {} to {}",
                    download.status, poll_result.status
                ),
                Some(serde_json::json!({
                    "old_status": download.status,
                    "new_status": poll_result.status,
                })),
            )
            .await;

            // Broadcast status change
            broadcast_download_updated(&state.pool, &state.download_events, download.id).await;

            // Dispatch to registered webhooks
            let event_name = format!("download.{}", poll_result.status);
            crate::webhook_dispatch::dispatch_download_event(
                &state.pool,
                &state.http_client,
                &event_name,
                download.id,
                &poll_result.status,
                Some(&download.name),
            )
            .await;

            if (poll_result.status == "completed" || poll_result.status == "failed")
                && download.callback_sent_at.is_none()
            {
                if poll_result.status == "completed" {
                    // For completed downloads, expand archives to list their contents
                    let expanded_files = if !poll_result.files.is_empty() {
                        expand_archives(
                            poll_result.files.clone(),
                            state.unrar_path.as_deref(),
                            state.sevenz_path.as_deref(),
                        )
                        .await
                    } else {
                        poll_result.files.clone()
                    };

                    // Persist scanned files for callback retries
                    if !expanded_files.is_empty()
                        && let Ok(files_json) = serde_json::to_value(&expanded_files)
                    {
                        let _ = downloads_repo::update_files_json(
                            &state.pool,
                            download.id,
                            &files_json,
                        )
                        .await;
                    }

                    // Enqueue metadata extraction job (async, non-blocking)
                    trigger_metadata_extraction(&state, &download).await;
                } else {
                    // Failed downloads: send callback immediately (no files to enrich)
                    send_failed_callback(&state, &download).await;
                    // Trigger re-search for the watch pattern so we find another release
                    trigger_research_for_download(&state, download.id).await;
                }
            }
        }

        // Retry pending submissions
        retry_pending_submissions(&state).await;

        // Retry pending watch callbacks
        crate::watch::service::retry_pending_watch_callbacks(&state).await;

        sleep(Duration::from_secs(15)).await;
    }
}

/// Exclude the failed release and immediately re-search for a replacement.
///
/// 1. Adds the failed release to the pattern's `excluded_releases` so all
///    future searches (RSS, backlog, manual) skip it.
/// 2. Runs an immediate backlog search to find the next best release.
/// 3. Resets `last_backlog_search_at` as a fallback in case the immediate
///    search finds nothing (the periodic scheduler will retry later).
pub async fn trigger_research_for_download(state: &AppState, download_id: Uuid) {
    let (pattern_id, release_title) = match watch_repo::get_match_info_for_download(
        &state.pool,
        download_id,
    )
    .await
    {
        Ok(Some(info)) => info,
        Ok(None) => return, // No watch match — manual download, nothing to re-search
        Err(e) => {
            warn!(download_id = %download_id, error = %e, "Failed to look up watch match for re-search");
            return;
        }
    };

    // Exclude the failed release so it won't be picked again
    let exclusion = serde_json::json!([{ "release_title": release_title }]);
    if let Err(e) = watch_repo::append_excluded_release(&state.pool, pattern_id, &exclusion).await {
        warn!(pattern_id = %pattern_id, error = %e, "Failed to exclude failed release");
    }

    // Reset backlog timestamp as fallback for the periodic scheduler
    if let Err(e) = watch_repo::reset_backlog_search(&state.pool, pattern_id).await {
        warn!(pattern_id = %pattern_id, error = %e, "Failed to reset backlog search for re-search");
    }

    log_event(
        &state.pool,
        download_id,
        "research_triggered",
        &format!("Re-search triggered after download failure, excluded: {release_title}"),
        Some(serde_json::json!({ "watch_pattern_id": pattern_id, "excluded_release": release_title })),
    )
    .await;

    info!(download_id = %download_id, pattern_id = %pattern_id, release = %release_title, "Excluded failed release, running immediate re-search");

    // Run immediate search — fetch the pattern with updated excluded_releases
    let pattern = match watch_repo::get_pattern_by_id(&state.pool, pattern_id).await {
        Ok(Some(p)) => p,
        Ok(None) => return,
        Err(e) => {
            warn!(pattern_id = %pattern_id, error = %e, "Failed to fetch pattern for immediate re-search");
            return;
        }
    };

    // Serialise against any concurrent trigger for the same pattern. Blocking:
    // research must not drop the work, and the periodic backlog scheduler has
    // try-lock semantics so it will happily skip while we hold the lock.
    let _lock = state.pattern_search_locks.acquire(pattern_id).await;

    // Rejection-driven re-search: bypass upgrade eligibility so a replacement
    // that doesn't out-score the failed grab is still allowed, but keep the
    // duplicate gate — this path is automated and must not stack a second copy
    // on top of one still downloading or already imported.
    let found = crate::jobs::handlers::backlog_search_pattern(
        state,
        &pattern,
        crate::jobs::handlers::SearchGates::REPLACEMENT,
    )
    .await;
    if found {
        info!(pattern_id = %pattern_id, "Immediate re-search found replacement release");
    } else {
        info!(pattern_id = %pattern_id, "Immediate re-search found no matches, backlog scheduler will retry");
    }
}

/// Lightweight version of [`trigger_research_for_download`] that only needs
/// pool + nats (no full AppState). Excludes the failed release and resets the
/// backlog timestamp so the periodic scheduler will retry. Does NOT run an
/// immediate search (that requires AppState for indexer clients).
pub async fn trigger_research_for_download_from_pool(
    pool: &sqlx::PgPool,
    _nats: &crate::nats::NatsState,
    download_id: Uuid,
) {
    let (pattern_id, release_title) = match watch_repo::get_match_info_for_download(
        pool,
        download_id,
    )
    .await
    {
        Ok(Some(info)) => info,
        Ok(None) => return,
        Err(e) => {
            warn!(download_id = %download_id, error = %e, "Failed to look up watch match for re-search");
            return;
        }
    };

    let exclusion = serde_json::json!([{ "release_title": release_title }]);
    if let Err(e) = watch_repo::append_excluded_release(pool, pattern_id, &exclusion).await {
        warn!(pattern_id = %pattern_id, error = %e, "Failed to exclude failed release");
    }

    if let Err(e) = watch_repo::reset_backlog_search(pool, pattern_id).await {
        warn!(pattern_id = %pattern_id, error = %e, "Failed to reset backlog search for re-search");
    }

    log_event(
        pool,
        download_id,
        "research_triggered",
        &format!("Re-search triggered after content validation failure, excluded: {release_title}"),
        Some(serde_json::json!({ "watch_pattern_id": pattern_id, "excluded_release": release_title })),
    )
    .await;

    info!(download_id = %download_id, pattern_id = %pattern_id, release = %release_title,
        "Excluded failed release after content validation, backlog scheduler will retry");
}

/// Retry downloads stuck in pending_retry status.
/// These are downloads where the download client was unavailable at submission time.
async fn retry_pending_submissions(state: &AppState) {
    let pending = downloads_repo::fetch_pending_retry_downloads(&state.pool)
        .await
        .unwrap_or_else(|e| {
            error!(error = %e, "failed to fetch pending retry downloads");
            Vec::new()
        });

    for download in pending {
        let new_attempts = download.submission_attempts + 1;

        // Get download client
        let client = match crate::download_clients::select_download_client(
            &state.pool,
            Some(download.download_client_id),
            &download.protocol,
        )
        .await
        {
            Ok(client) => client,
            Err(_) => {
                warn!(
                    download_id = %download.id,
                    "Could not find download client for retry"
                );
                let _ = downloads_repo::update_submission_attempt(
                    &state.pool,
                    download.id,
                    new_attempts,
                    Some("download client not found or disabled"),
                )
                .await;
                continue;
            }
        };

        let handler = match state.download_clients.get(client.client_type.as_str()) {
            Some(h) => h,
            None => {
                let error = "unsupported download client";
                let _ =
                    downloads_repo::mark_submission_failed(&state.pool, download.id, error).await;
                continue;
            }
        };

        // Fetch release payload and attempt submission
        let result = crate::services::downloads::retry_submission(
            &state.http_client,
            &download,
            &client,
            handler.as_ref(),
        )
        .await;

        match result {
            Ok(external_id) => {
                info!(
                    download_id = %download.id,
                    external_id = %external_id,
                    attempt = new_attempts,
                    "Submission retry succeeded"
                );
                let _ =
                    downloads_repo::mark_submission_success(&state.pool, download.id, &external_id)
                        .await;

                crate::helpers::log_event(
                    &state.pool,
                    download.id,
                    "submission_retry_success",
                    &format!("Download client now available (attempt {})", new_attempts),
                    Some(serde_json::json!({
                        "external_id": external_id,
                        "attempt": new_attempts,
                    })),
                )
                .await;

                crate::helpers::broadcast_download_updated(
                    &state.pool,
                    &state.download_events,
                    download.id,
                )
                .await;
            }
            Err(e) => {
                let _ = downloads_repo::update_submission_attempt(
                    &state.pool,
                    download.id,
                    new_attempts,
                    Some(&e.to_string()),
                )
                .await;

                if new_attempts >= 10 {
                    let error = format!(
                        "Download client unavailable after {} attempts: {}",
                        new_attempts, e
                    );
                    warn!(download_id = %download.id, error = %error, "Submission retry exhausted");
                    let _ =
                        downloads_repo::mark_submission_failed(&state.pool, download.id, &error)
                            .await;

                    crate::helpers::log_event(
                        &state.pool,
                        download.id,
                        "submission_retry_exhausted",
                        &format!("Max retry attempts reached ({})", new_attempts),
                        Some(serde_json::json!({
                            "error": e.to_string(),
                            "attempts": new_attempts,
                        })),
                    )
                    .await;
                } else {
                    info!(
                        download_id = %download.id,
                        attempt = new_attempts,
                        error = %e,
                        "Submission retry failed, will retry later"
                    );
                }

                crate::helpers::broadcast_download_updated(
                    &state.pool,
                    &state.download_events,
                    download.id,
                )
                .await;
            }
        }
    }
}
