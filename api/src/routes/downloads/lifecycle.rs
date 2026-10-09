use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use serde::Serialize;
use tracing::warn;
use uuid::Uuid;

use serde::Deserialize;

use crate::auth::require_auth;
use crate::download_clients::{expand_archives, select_download_client};
use crate::helpers::{broadcast_download_updated, log_event};
use crate::jobs;
use crate::jobs::handlers::build_callback_from_enriched;
use crate::parsing::ParsedRelease;
use crate::polling::trigger_research_for_download;
use crate::repositories::downloads as downloads_repo;
use crate::repositories::watch as watch_repo;
use crate::state::AppState;
use crate::types::CompleteDownloadRequest;

use super::{ReingestRequest, ReingestResponse};

pub async fn complete_download(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(payload): Json<CompleteDownloadRequest>,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let callback_info = match downloads_repo::complete_download_status(
        &state.pool,
        id,
        &payload.status,
        Some(&payload.file_path),
    )
    .await
    {
        Ok(Some(info)) => info,
        Ok(None) => return (StatusCode::NOT_FOUND, "not found").into_response(),
        Err(err) => {
            warn!(error = %err, "Failed to update download");
            return (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response();
        }
    };

    let (callback_url, metadata, auth_token) = (
        callback_info.callback_url,
        callback_info.metadata,
        callback_info.auth_token,
    );

    let payload = nats_jobs::DownloadCompletedEvent {
        download_id: id,
        status: payload.status,
        name: String::new(),
        metadata,
        files: vec![], // Manual completion doesn't provide file list
        file_groups: None,
        release_interpretation: None, // Manual completion doesn't have parsing context
        error: None,
        recoverable_error: None,
        idempotency_key: None, // Manual completions don't use idempotency
        is_upgrade: false,
        quality_score: None,
        replaces_download_id: None,
        release_info: None, // Manual completion has no release data
    };

    let mut request = state.http_client.post(callback_url).json(&payload);
    if let Some(token) = auth_token {
        request = request.header("Authorization", format!("Bearer {}", token));
    }
    if let Ok(resp) = request.send().await
        && resp.status().is_success()
    {
        let _ = downloads_repo::mark_callback_sent(&state.pool, id).await;
    }

    // Broadcast download updated event
    broadcast_download_updated(&state.pool, &state.download_events, id).await;

    StatusCode::NO_CONTENT.into_response()
}

pub async fn confirm_import(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    // Get download info including the download client
    let import_info = match downloads_repo::confirm_import(&state.pool, id).await {
        Ok(Some(info)) => info,
        Ok(None) => return (StatusCode::NOT_FOUND, "not found").into_response(),
        Err(err) => {
            warn!(error = %err, "Failed to confirm import");
            return (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response();
        }
    };

    let (client_id, external_id, protocol, client_data) = (
        import_info.download_client_id,
        import_info.external_id,
        import_info.protocol,
        import_info.client_data,
    );

    // Mark the watch match as import-confirmed (survives download deletion)
    if let Err(e) = watch_repo::mark_match_import_confirmed(&state.pool, id).await {
        warn!(error = %e, download_id = %id, "Failed to mark watch match import confirmed");
    }

    // Log import confirmed event
    log_event(
        &state.pool,
        id,
        "import_confirmed",
        "Kobbler confirmed import",
        None,
    )
    .await;

    // If we have client info, delete from download client
    if let (Some(client_id), Some(external_id)) = (client_id, external_id)
        && let Ok(client) = select_download_client(&state.pool, Some(client_id), &protocol).await
        && client.delete_after_import
    {
        let remove_result = match state.download_clients.get(client.client_type.as_str()) {
            Some(handler) => {
                handler
                    .remove(&state.http_client, &client, &external_id, true)
                    .await
            }
            None => Ok(()),
        };
        if let Err(err) = remove_result {
            log_event(
                &state.pool,
                id,
                "remove_failed",
                "Failed to remove from download client after import",
                Some(serde_json::json!({ "error": err.to_string() })),
            )
            .await;
        } else {
            log_event(
                &state.pool,
                id,
                "removed",
                "Removed from download client after import confirmation",
                Some(serde_json::json!({ "external_id": external_id })),
            )
            .await;
        }
    }

    // Filesystem cleanup: delete the storage directory if it still exists.
    // SABnzbd's del_files flag is unreliable — this is the fallback.
    if let Some(ref cd) = client_data
        && let Some(storage) = cd.get("storage").and_then(|v| v.as_str())
        && !storage.is_empty()
    {
        let storage_path = std::path::Path::new(storage);
        // SABnzbd's storage field can be a file path for single-file downloads;
        // resolve to the parent directory so we clean up the whole download folder.
        let cleanup_dir = if storage_path.is_file() {
            match storage_path.parent() {
                Some(parent) => parent.to_path_buf(),
                None => {
                    tracing::warn!(
                        download_id = %id,
                        path = storage,
                        "Cannot determine parent directory of storage file"
                    );
                    return StatusCode::NO_CONTENT.into_response();
                }
            }
        } else {
            storage_path.to_path_buf()
        };
        // Safety: refuse to delete anything that looks like a system/root path.
        // Must be at least 3 components deep (e.g. /downloads/complete/SomeRelease)
        let depth = cleanup_dir.components().count();
        if depth < 3 {
            tracing::warn!(
                download_id = %id,
                path = %cleanup_dir.display(),
                depth,
                "Refusing to delete storage path: too shallow (need at least 3 components)"
            );
        } else if cleanup_dir.exists() {
            if let Err(err) = tokio::fs::remove_dir_all(&cleanup_dir).await {
                tracing::warn!(
                    download_id = %id,
                    path = %cleanup_dir.display(),
                    error = %err,
                    "Failed to clean up storage directory after import"
                );
            } else {
                tracing::info!(
                    download_id = %id,
                    path = %cleanup_dir.display(),
                    "Cleaned up storage directory after import"
                );
            }
        }
    }

    // Broadcast download updated event
    broadcast_download_updated(&state.pool, &state.download_events, id).await;

    StatusCode::NO_CONTENT.into_response()
}

#[derive(Debug, Deserialize)]
pub struct ImportFailedRequest {
    pub error: Option<String>,
}

pub async fn import_failed(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(payload): Json<ImportFailedRequest>,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    // Guard: don't overwrite a successful import with a late-arriving failure
    match downloads_repo::is_import_confirmed(&state.pool, id).await {
        Ok(true) => {
            tracing::warn!(
                download_id = %id,
                error = ?payload.error,
                "Ignoring import_failed for already-confirmed download (likely redelivery race)"
            );
            return StatusCode::NO_CONTENT.into_response();
        }
        Err(e) => {
            tracing::warn!(
                download_id = %id,
                error = %e,
                "Failed to check import confirmation, proceeding with failure handling"
            );
        }
        _ => {}
    }

    let reason = payload
        .error
        .as_deref()
        .unwrap_or("Import rejected by Kobbler");

    // Claim the failure transition so a retried/duplicated request logs one
    // failure and fires one re-search. See `claim_failure`.
    match downloads_repo::claim_failure(&state.pool, id, reason).await {
        Ok(false) => {
            tracing::warn!(
                download_id = %id,
                error = ?payload.error,
                "Ignoring duplicate import_failed for already-failed download"
            );
            return StatusCode::NO_CONTENT.into_response();
        }
        Err(e) => {
            tracing::error!(download_id = %id, error = %e, "Failed to mark download as failed after import rejection");
            return StatusCode::NO_CONTENT.into_response();
        }
        Ok(true) => {}
    }

    log_event(
        &state.pool,
        id,
        "import_failed",
        "Kobbler reported import failure",
        payload
            .error
            .as_ref()
            .map(|e| serde_json::json!({ "error": e })),
    )
    .await;

    // Trigger re-search for the watch pattern
    trigger_research_for_download(&state, id).await;

    // Broadcast download updated event
    broadcast_download_updated(&state.pool, &state.download_events, id).await;

    StatusCode::NO_CONTENT.into_response()
}

pub async fn import_deferred(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(payload): Json<ImportFailedRequest>,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    log_event(
        &state.pool,
        id,
        "import_deferred",
        "Media root temporarily unavailable — will import automatically when available",
        payload
            .error
            .as_ref()
            .map(|e| serde_json::json!({ "reason": e })),
    )
    .await;

    // Do NOT trigger re-search — this is temporary, not a failure
    broadcast_download_updated(&state.pool, &state.download_events, id).await;

    StatusCode::NO_CONTENT.into_response()
}

pub async fn reingest_download(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(payload): Json<ReingestRequest>,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    // Fetch the download record
    let reingest_info = match downloads_repo::get_reingest_info(&state.pool, id).await {
        Ok(Some(info)) => info,
        Ok(None) => return (StatusCode::NOT_FOUND, "not found").into_response(),
        Err(err) => {
            warn!(error = %err, "Failed to fetch download for reingest");
            return (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response();
        }
    };

    let mut callback_url = reingest_info.callback_url;
    let mut auth_token = reingest_info.auth_token;
    let metadata = reingest_info.metadata;
    let files_json = reingest_info.files_json;

    // Only allow reingest for completed or failed downloads.
    // For the callback, always use "completed" — the user is explicitly asking
    // to re-attempt import. The download itself may have been marked "failed"
    // because the original import failed (e.g. target was down), not because
    // the download content is bad.
    let raw_status = reingest_info.status.clone();
    if raw_status != "completed" && raw_status != "failed" {
        return (
            StatusCode::BAD_REQUEST,
            Json(ReingestResponse {
                success: false,
                message: format!(
                    "Cannot reingest download with status '{}'. Must be completed or failed.",
                    raw_status
                ),
            }),
        )
            .into_response();
    }

    // Update callback_url and/or auth_token if provided
    if let Some(new_url) = &payload.callback_url {
        callback_url = new_url.clone();
    }
    if let Some(new_token) = &payload.auth_token {
        auth_token = Some(new_token.clone());
    }

    // Update the database with new values and reset callback tracking
    if let Err(err) = downloads_repo::reset_callback_tracking(
        &state.pool,
        id,
        &callback_url,
        auth_token.as_deref(),
    )
    .await
    {
        warn!(error = %err, "Failed to update download for reingest");
        return (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response();
    }

    // Log the reingest attempt
    log_event(
        &state.pool,
        id,
        "reingest_started",
        "Reingest triggered",
        Some(serde_json::json!({
            "callback_url": callback_url,
            "url_updated": payload.callback_url.is_some(),
            "token_updated": payload.auth_token.is_some(),
        })),
    )
    .await;

    // Parse files_json back to ScannedFile
    let scanned_files: Vec<crate::download_clients::ScannedFile> = files_json
        .and_then(|json| serde_json::from_value(json).ok())
        .unwrap_or_default();

    // Check if files have enrichment data already
    let files_enriched = scanned_files.first().is_some_and(|f| f.is_enriched());

    if !files_enriched && !scanned_files.is_empty() {
        // Files aren't enriched yet -- enqueue metadata extraction job
        if state.nats.is_available() {
            let payload = serde_json::json!({ "download_id": id.to_string() });
            let context = jobs::JobContext {
                entity_type: Some("download".to_string()),
                entity_id: Some(id),
                description: format!("Metadata extraction for reingest of {}", reingest_info.name),
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
                log_event(
                    &state.pool,
                    id,
                    "reingest_queued",
                    "Metadata extraction queued for reingest",
                    Some(serde_json::json!({ "job_id": job_id.to_string() })),
                )
                .await;
                return Json(ReingestResponse {
                    success: true,
                    message: "Metadata extraction queued, callback will be sent after enrichment"
                        .to_string(),
                })
                .into_response();
            }
        }
        // NATS unavailable or enqueue failed -- fall through to send unenriched callback
    }

    // Build callback from enriched data (or unenriched if no files / NATS unavailable)
    let release_interpretation: Option<ParsedRelease> = reingest_info
        .release_interpretation_json
        .and_then(|json| serde_json::from_value(json).ok());

    let callback_payload = build_callback_from_enriched(
        id,
        &reingest_info.name,
        "completed", // Always "completed" for reingest — see comment above
        &metadata,
        &scanned_files,
        release_interpretation.as_ref(),
        None, // Clear error for reingest — we're retrying, not reporting failure
        None,
        None, // Reingest uses fresh callback, no idempotency key
        reingest_info.release_payload,
    );

    // Send the callback
    let mut request = state
        .http_client
        .post(&callback_url)
        .json(&callback_payload);
    if let Some(token) = &auth_token {
        request = request.header("Authorization", format!("Bearer {}", token));
    }

    match request.send().await {
        Ok(resp) if resp.status().is_success() => {
            let _ = downloads_repo::mark_callback_sent(&state.pool, id).await;
            log_event(
                &state.pool,
                id,
                "callback_sent",
                "Reingest callback succeeded",
                Some(serde_json::json!({
                    "url": callback_url,
                    "status": resp.status().as_u16(),
                })),
            )
            .await;
            Json(ReingestResponse {
                success: true,
                message: "Callback sent successfully".to_string(),
            })
            .into_response()
        }
        Ok(resp) => {
            let status_code = resp.status();
            let body = resp.text().await.ok();
            log_event(
                &state.pool,
                id,
                "callback_failed",
                "Reingest callback failed",
                Some(serde_json::json!({
                    "url": callback_url,
                    "status": status_code.as_u16(),
                    "response_body": body,
                })),
            )
            .await;
            Json(ReingestResponse {
                success: false,
                message: format!("Callback failed with status {}", status_code),
            })
            .into_response()
        }
        Err(err) => {
            log_event(
                &state.pool,
                id,
                "callback_failed",
                "Reingest callback request failed",
                Some(serde_json::json!({
                    "url": callback_url,
                    "error": err.to_string(),
                })),
            )
            .await;
            Json(ReingestResponse {
                success: false,
                message: format!("Request failed: {}", err),
            })
            .into_response()
        }
    }
}

#[derive(Debug, Serialize)]
pub struct RescanResponse {
    pub success: bool,
    pub message: String,
    pub files_count: usize,
}

/// Rescan files for a completed download and re-trigger callback.
///
/// This re-polls the download client, re-expands archives (to pick up nested
/// files like split RARs), updates files_json, and sends a new callback.
pub async fn rescan_download(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    // Get download info
    let rescan_info = match downloads_repo::get_rescan_info(&state.pool, id).await {
        Ok(Some(info)) => info,
        Ok(None) => return (StatusCode::NOT_FOUND, "not found").into_response(),
        Err(err) => {
            warn!(error = %err, "Failed to fetch download for rescan");
            return (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response();
        }
    };

    // Only allow rescan for completed downloads
    if rescan_info.status != "completed" {
        return (
            StatusCode::BAD_REQUEST,
            Json(RescanResponse {
                success: false,
                message: format!(
                    "Cannot rescan download with status '{}'. Must be completed.",
                    rescan_info.status
                ),
                files_count: 0,
            }),
        )
            .into_response();
    }

    // Need client info to re-poll
    let (Some(client_id), Some(external_id)) = (
        rescan_info.download_client_id,
        rescan_info.external_id.as_ref(),
    ) else {
        return (
            StatusCode::BAD_REQUEST,
            Json(RescanResponse {
                success: false,
                message: "Download has no associated client, cannot rescan".to_string(),
                files_count: 0,
            }),
        )
            .into_response();
    };

    // Select download client
    let client =
        match select_download_client(&state.pool, Some(client_id), &rescan_info.protocol).await {
            Ok(client) => client,
            Err(resp) => return resp,
        };

    // Get handler for client type
    let handler = match state.download_clients.get(client.client_type.as_str()) {
        Some(handler) => handler,
        None => {
            return (
                StatusCode::BAD_REQUEST,
                Json(RescanResponse {
                    success: false,
                    message: format!("Unsupported client type: {}", client.client_type),
                    files_count: 0,
                }),
            )
                .into_response();
        }
    };

    // Re-poll the download client
    let poll_result = match handler.poll(&state.http_client, &client, external_id).await {
        Ok(result) => result,
        Err(err) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(RescanResponse {
                    success: false,
                    message: format!("Failed to poll download client: {}", err),
                    files_count: 0,
                }),
            )
                .into_response();
        }
    };

    // Expand archives (this now handles split RARs)
    let expanded_files = expand_archives(
        poll_result.files.clone(),
        state.unrar_path.as_deref(),
        state.sevenz_path.as_deref(),
    )
    .await;

    let files_count = expanded_files.len();

    // Update files_json in database
    let files_json = serde_json::to_value(&expanded_files).ok();
    if let Some(ref json) = files_json
        && let Err(err) = downloads_repo::update_files_json(&state.pool, id, json).await
    {
        warn!(error = %err, "Failed to update files_json");
    }

    // Log the rescan
    log_event(
        &state.pool,
        id,
        "files_rescanned",
        &format!("Files rescanned: {} files found", files_count),
        Some(serde_json::json!({
            "files_count": files_count,
        })),
    )
    .await;

    // Reset callback tracking so the callback will be sent after enrichment
    let reingest_info = match downloads_repo::get_reingest_info(&state.pool, id).await {
        Ok(Some(info)) => info,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                Json(RescanResponse {
                    success: false,
                    message: "Download not found after rescan".to_string(),
                    files_count,
                }),
            )
                .into_response();
        }
        Err(err) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(RescanResponse {
                    success: false,
                    message: format!("Database error: {}", err),
                    files_count,
                }),
            )
                .into_response();
        }
    };

    if let Err(err) = downloads_repo::reset_callback_tracking(
        &state.pool,
        id,
        &reingest_info.callback_url,
        reingest_info.auth_token.as_deref(),
    )
    .await
    {
        warn!(error = %err, "Failed to reset callback tracking for rescan");
    }

    // Enqueue metadata extraction job (enriches files + sends callback)
    let mut queued = false;
    if state.nats.is_available() {
        let payload = serde_json::json!({ "download_id": id.to_string() });
        let context = jobs::JobContext {
            entity_type: Some("download".to_string()),
            entity_id: Some(id),
            description: format!("Metadata extraction after rescan of {}", rescan_info.name),
            display: None,
        };

        if jobs::enqueue_optional(
            &state.pool,
            state.nats.publisher(),
            "metadata_extraction",
            payload,
            context,
        )
        .await
        .is_some()
        {
            queued = true;
        }
    }

    // Broadcast update
    broadcast_download_updated(&state.pool, &state.download_events, id).await;

    Json(RescanResponse {
        success: true,
        message: if queued {
            format!(
                "Rescan complete, {} files found, metadata extraction queued",
                files_count
            )
        } else {
            format!(
                "Rescan complete, {} files found (metadata extraction will run on next poll)",
                files_count
            )
        },
        files_count,
    })
    .into_response()
}
