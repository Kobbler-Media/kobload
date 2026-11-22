use sqlx::PgPool;
use tracing::{debug, error, warn};
use uuid::Uuid;

use crate::download_clients::{DownloadClientResolver, client_of_download};
use crate::helpers::{broadcast_download_updated, log_event};
use crate::polling::trigger_research_for_download;
use crate::repositories::Result;
use crate::repositories::downloads as downloads_repo;
use crate::repositories::watch as watch_repo;
use crate::state::AppState;

/// `Ok(false)` when the download is unknown, already confirmed, or already failed.
pub async fn confirm_import(state: &AppState, download_id: Uuid) -> Result<bool> {
    if !downloads_repo::confirm_import(&state.pool, download_id).await? {
        return Ok(false);
    }

    if let Err(e) = watch_repo::mark_match_import_confirmed(&state.pool, download_id).await {
        warn!(error = %e, download_id = %download_id, "Failed to mark watch match import confirmed");
    }

    log_event(
        &state.pool,
        download_id,
        "import_confirmed",
        "Kobbler confirmed import",
        None,
    )
    .await;

    release_from_client(
        &state.pool,
        &state.http_client,
        state.download_clients.as_ref(),
        download_id,
        Ended::Imported,
    )
    .await;

    broadcast_download_updated(&state.pool, &state.download_events, download_id).await;
    Ok(true)
}

pub async fn fail_import(state: &AppState, download_id: Uuid, error: Option<&str>) {
    // A NATS redelivery can report a failure after the import already succeeded.
    match downloads_repo::is_import_confirmed(&state.pool, download_id).await {
        Ok(true) => {
            warn!(download_id = %download_id, error, "Ignoring import_failed for already-confirmed download");
            return;
        }
        Err(e) => {
            warn!(download_id = %download_id, error = %e, "Failed to check import confirmation, proceeding with failure handling");
        }
        Ok(false) => {}
    }

    let reason = error.unwrap_or("Import rejected by Kobbler");

    // Each extra re-search starts an independent replacement chain, so a repeat
    // import_failed must not fire a second one.
    match downloads_repo::claim_failure(&state.pool, download_id, reason).await {
        Ok(false) => {
            warn!(download_id = %download_id, error, "Ignoring duplicate import_failed for already-failed download");
            return;
        }
        Err(e) => {
            error!(download_id = %download_id, error = %e, "Failed to mark download as failed after import rejection");
            return;
        }
        Ok(true) => {}
    }

    log_event(
        &state.pool,
        download_id,
        "import_failed",
        "Kobbler reported import failure",
        error.map(|e| serde_json::json!({ "error": e })),
    )
    .await;

    release_from_client(
        &state.pool,
        &state.http_client,
        state.download_clients.as_ref(),
        download_id,
        Ended::Failed,
    )
    .await;

    trigger_research_for_download(state, download_id).await;

    broadcast_download_updated(&state.pool, &state.download_events, download_id).await;
}

pub async fn retry_import(state: &AppState, download_id: Uuid, error: Option<&str>) {
    let reason = error.unwrap_or("Kobbler could not import the download");
    match downloads_repo::schedule_import_retry(&state.pool, download_id, reason).await {
        Ok(Some(attempts)) => {
            log_event(
                &state.pool,
                download_id,
                "import_retry_scheduled",
                "Kobbler could not import for a reason outside the release, ingest will be resent",
                Some(serde_json::json!({ "error": reason, "attempts": attempts })),
            )
            .await;
        }
        Ok(None) => {
            let message = format!(
                "Import kept failing after {} attempts: {reason}",
                downloads_repo::MAX_CALLBACK_ATTEMPTS
            );
            match downloads_repo::exhaust_import_retries(&state.pool, download_id, &message).await {
                Ok(true) => {
                    log_event(
                        &state.pool,
                        download_id,
                        "import_retry_exhausted",
                        &message,
                        None,
                    )
                    .await;
                }
                Ok(false) => {
                    debug!(download_id = %download_id, "Ignoring import retry for a download not awaiting import");
                    return;
                }
                Err(e) => {
                    error!(download_id = %download_id, error = %e, "Failed to fail download after exhausted import retries");
                    return;
                }
            }
        }
        Err(e) => {
            error!(download_id = %download_id, error = %e, "Failed to schedule import retry");
            return;
        }
    }

    broadcast_download_updated(&state.pool, &state.download_events, download_id).await;
}

pub async fn defer_import(state: &AppState, download_id: Uuid, reason: Option<&str>) {
    log_event(
        &state.pool,
        download_id,
        "import_deferred",
        "Media root temporarily unavailable — will import automatically when available",
        reason.map(|r| serde_json::json!({ "reason": r })),
    )
    .await;

    broadcast_download_updated(&state.pool, &state.download_events, download_id).await;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ended {
    Imported,
    Failed,
}

pub async fn release_from_client(
    pool: &PgPool,
    http_client: &reqwest::Client,
    download_clients: &dyn DownloadClientResolver,
    download_id: Uuid,
    ended: Ended,
) {
    let info = match downloads_repo::get_deletion_info(pool, download_id).await {
        Ok(Some(info)) => info,
        Ok(None) => return,
        Err(e) => {
            warn!(download_id = %download_id, error = %e, "Failed to load download for client release");
            return;
        }
    };
    let (Some(client_id), Some(external_id)) = (info.download_client_id, info.external_id) else {
        return;
    };
    let Ok(client) = client_of_download(pool, client_id).await else {
        return;
    };
    // The usenet engine keeps a job, and its folder, until it is released; left alone they
    // pile up the way SABnzbd's history did.
    let engine_job = client.client_type == "usenet";
    if !client.delete_after_import && !engine_job {
        return;
    }
    let delete_data = client.delete_after_import || ended == Ended::Failed;
    let Some(handler) = download_clients.get(client.client_type.as_str()) else {
        debug!(download_id = %download_id, client_type = %client.client_type, "No handler to release download");
        return;
    };

    match handler
        .remove(http_client, &client, &external_id, delete_data)
        .await
    {
        Ok(()) => {
            log_event(
                pool,
                download_id,
                "removed",
                "Removed from download client",
                Some(serde_json::json!({ "external_id": external_id })),
            )
            .await;
        }
        Err(err) => {
            log_event(
                pool,
                download_id,
                "remove_failed",
                "Failed to remove from download client",
                Some(serde_json::json!({ "error": err.to_string() })),
            )
            .await;
        }
    }
}
