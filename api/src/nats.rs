//! NATS JetStream integration for cross-service communication.

use std::sync::Arc;

use nats_jobs::{
    DownloadCompletedEvent, JobHandler as NatsJobHandler, JobStatusDb, NatsClient, NatsConfig,
    OptionalPublisher, Publisher, ScheduledJob, Scheduler, WorkerConfig,
};
use tokio::sync::watch;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::jobs::handlers::{
    BacklogSearchHandler, BitmagnetSyncHandler, CacheCleanupHandler, DefinitionUpdateHandler,
    GrabHeldMatchHandler, HashIndexBootstrapHandler, HashIndexImportHandler, IndexerSyncHandler,
    JobCleanupHandler, MetadataExtractionHandler, RefreshIndexerCapsHandler,
    SearchPatternNowHandler, SearchRequestHandler, TrashFilterSyncHandler, UpgradeCleanupHandler,
    WantRequestHandler, WatchDirectoryNotifyHandler, WatchDirectoryScanCheckHandler,
    WatchDirectoryScanHandler, WatchPatternDeleteHandler,
};
use crate::jobs::worker::JobHandler;

/// NATS state for the application.
#[derive(Clone)]
pub struct NatsState {
    /// The NATS client (optional - may not be configured).
    pub client: Option<NatsClient>,
    /// Publisher for enqueuing jobs and publishing events.
    pub publisher: OptionalPublisher,
}

impl NatsState {
    /// Create a new NATS state with optional connection.
    pub fn new(client: Option<NatsClient>) -> Self {
        let publisher = OptionalPublisher::new(client.clone(), "kobload");
        Self { client, publisher }
    }

    /// Check if NATS is available.
    pub fn is_available(&self) -> bool {
        self.client.is_some()
    }

    /// Get the publisher (may be unavailable).
    pub fn publisher(&self) -> &OptionalPublisher {
        &self.publisher
    }
}

/// Initialize NATS connection.
pub async fn init_nats() -> Option<NatsClient> {
    let config = NatsConfig::from_env("kobload");

    match NatsClient::try_connect(&config).await {
        Some(client) => {
            info!("Connected to NATS at {}", config.url);
            Some(client)
        }
        None => {
            warn!(
                "NATS not available at {}. Running without cross-service communication.",
                config.url
            );
            None
        }
    }
}

/// Adapter to use Kobload's JobHandler trait with nats-jobs.
struct HandlerAdapter<H: JobHandler> {
    inner: H,
}

impl<H: JobHandler> HandlerAdapter<H> {
    fn new(handler: H) -> Self {
        Self { inner: handler }
    }
}

#[async_trait::async_trait]
impl<H: JobHandler + 'static> NatsJobHandler for HandlerAdapter<H> {
    fn job_type(&self) -> &'static str {
        self.inner.job_type()
    }

    fn max_concurrent(&self) -> usize {
        self.inner.max_concurrent()
    }

    async fn execute(
        &self,
        job_id: Uuid,
        payload: serde_json::Value,
        cancel: tokio_util::sync::CancellationToken,
    ) -> Result<serde_json::Value, nats_jobs::JobError> {
        self.inner
            .execute(job_id, payload, cancel)
            .await
            .map_err(|e| match e {
                crate::jobs::JobError::Cancelled => nats_jobs::JobError::Cancelled,
                e if e.is_retryable() => nats_jobs::JobError::retryable(e.to_string()),
                e => nats_jobs::JobError::permanent(e.to_string()),
            })
    }
}

/// Start the NATS-based job worker.
pub async fn start_nats_worker(
    nats: NatsClient,
    state: &crate::state::AppState,
    shutdown_rx: watch::Receiver<bool>,
) {
    let job_db = JobStatusDb::new(state.pool.clone());
    let config = WorkerConfig::for_service("kobload");
    let nats_for_cleanup = nats.clone();

    let mut worker = nats_jobs::JobWorker::new(nats, config).with_job_db(job_db);

    // Register handlers using the adapter
    worker.register_handler(HandlerAdapter::new(WatchPatternDeleteHandler::new(
        state.clone(),
    )));
    worker.register_handler(HandlerAdapter::new(SearchRequestHandler::new(
        state.pool.clone(),
        state.http_client.clone(),
        state.indexers.clone(),
    )));
    worker.register_handler(HandlerAdapter::new(WantRequestHandler::new(
        state.pool.clone(),
        state.nats.clone(),
        state.http_client.clone(),
        state.download_events.clone(),
        state.download_clients.clone(),
        state.indexers.clone(),
        state.xem_cache.clone(),
        state.scene_mappings.clone(),
        Arc::clone(&state.trash_filters),
        state.pattern_search_locks.clone(),
    )));

    worker.register_handler(HandlerAdapter::new(MetadataExtractionHandler::new(
        state.pool.clone(),
        state.nats.clone(),
        state.http_client.clone(),
        state.download_events.clone(),
    )));

    // Scheduled job handlers
    worker.register_handler(HandlerAdapter::new(IndexerSyncHandler::new(
        state.pool.clone(),
        state.http_client.clone(),
    )));
    worker.register_handler(HandlerAdapter::new(DefinitionUpdateHandler::new(
        state.http_client.clone(),
        state.definitions.clone(),
    )));
    worker.register_handler(HandlerAdapter::new(BacklogSearchHandler::new(
        state.clone(),
    )));
    worker.register_handler(HandlerAdapter::new(SearchPatternNowHandler::new(
        state.clone(),
    )));
    worker.register_handler(HandlerAdapter::new(GrabHeldMatchHandler::new(
        state.clone(),
    )));
    worker.register_handler(HandlerAdapter::new(CacheCleanupHandler::new(
        state.pool.clone(),
    )));
    worker.register_handler(HandlerAdapter::new(TrashFilterSyncHandler::new(
        state.pool.clone(),
        state.http_client.clone(),
        Arc::clone(&state.trash_filters),
    )));
    worker.register_handler(HandlerAdapter::new(UpgradeCleanupHandler::new(
        state.pool.clone(),
    )));
    worker.register_handler(HandlerAdapter::new(RefreshIndexerCapsHandler::new(
        state.pool.clone(),
        state.http_client.clone(),
    )));

    // Watch directory handlers
    worker.register_handler(HandlerAdapter::new(WatchDirectoryScanCheckHandler::new(
        state.pool.clone(),
        state.nats.clone(),
    )));
    worker.register_handler(HandlerAdapter::new(WatchDirectoryScanHandler::new(
        state.pool.clone(),
        state.nats.clone(),
    )));
    worker.register_handler(HandlerAdapter::new(WatchDirectoryNotifyHandler::new(state)));
    worker.register_handler(HandlerAdapter::new(JobCleanupHandler::new(
        state.pool.clone(),
        nats_for_cleanup,
    )));

    // Hash index import (triggered manually via API)
    worker.register_handler(HandlerAdapter::new(HashIndexImportHandler::new(
        state.pool.clone(),
    )));

    // Hash index bootstrap (upload + COPY via temp table)
    worker.register_handler(HandlerAdapter::new(HashIndexBootstrapHandler::new(
        state.pool.clone(),
    )));

    // BitMagnet DHT sync (scheduled every 15 minutes)
    worker.register_handler(HandlerAdapter::new(BitmagnetSyncHandler::new(
        state.pool.clone(),
        state.http_client.clone(),
    )));

    info!("Starting NATS job worker");
    if let Err(e) = worker.run(shutdown_rx).await {
        error!("NATS job worker error: {}", e);
    }
}

/// Start the scheduler for periodic jobs.
pub async fn start_scheduler(nats: NatsClient, shutdown_rx: watch::Receiver<bool>) {
    let publisher = Publisher::new(nats, "kobload");
    let mut scheduler = Scheduler::new(publisher);

    // Sync Prowlarr indexers hourly (with 5 minute initial delay to let system stabilize)
    scheduler.register(ScheduledJob::new(
        "indexer-sync",
        "kobload",
        "indexer_sync",
        std::time::Duration::from_secs(3600), // hourly
    ));

    // Update Cardigann definitions weekly
    scheduler.register(ScheduledJob::new(
        "definition-update",
        "kobload",
        "definition_update",
        std::time::Duration::from_secs(7 * 24 * 3600), // weekly
    ));

    // Backlog search every 5 minutes (processes 1-3 patterns per run)
    scheduler.register(ScheduledJob::new(
        "backlog-search",
        "kobload",
        "backlog_search",
        std::time::Duration::from_secs(300), // 5 minutes
    ));

    // Cache cleanup every 15 minutes
    scheduler.register(ScheduledJob::new(
        "cache-cleanup",
        "kobload",
        "cache_cleanup",
        std::time::Duration::from_secs(900), // 15 minutes
    ));

    scheduler.register(ScheduledJob::new(
        "trash-filter-sync",
        "kobload",
        "trash_filter_sync",
        std::time::Duration::from_secs(86400), // daily
    ));

    // Upgrade cleanup hourly (delete patterns with expired upgrade windows)
    scheduler.register(ScheduledJob::new(
        "upgrade-cleanup",
        "kobload",
        "upgrade_cleanup",
        std::time::Duration::from_secs(3600), // hourly
    ));

    // Refresh Newznab indexer capabilities daily
    scheduler.register(ScheduledJob::new(
        "refresh-indexer-caps",
        "kobload",
        "refresh_indexer_caps",
        std::time::Duration::from_secs(24 * 3600), // daily
    ));

    // Job cleanup hourly — reset orphaned running jobs, delete old records
    scheduler.register(
        ScheduledJob::new(
            "job-cleanup",
            "kobload",
            "job_cleanup",
            std::time::Duration::from_secs(3600), // hourly
        )
        .run_immediately(),
    );

    // Watch directory scan check every 5 minutes
    scheduler.register(ScheduledJob::new(
        "watch-directory-scan-check",
        "kobload",
        "watch_directory_scan_check",
        std::time::Duration::from_secs(300), // 5 minutes
    ));

    // BitMagnet DHT sync every 5 minutes
    scheduler.register(ScheduledJob::new(
        "bitmagnet-sync",
        "kobload",
        "bitmagnet_sync",
        std::time::Duration::from_secs(300), // 5 minutes
    ));

    info!("Starting NATS scheduler");
    scheduler.run(shutdown_rx).await;
}

/// Publish a download completed event.
pub async fn publish_download_completed(nats: &NatsState, event: &DownloadCompletedEvent) {
    if !nats.is_available() {
        return;
    }

    let payload = match serde_json::to_value(event) {
        Ok(p) => p,
        Err(e) => {
            error!("Failed to serialize download completed event: {}", e);
            return;
        }
    };

    nats.publisher()
        .publish_event("download.completed", payload)
        .await;
}

/// Publish a watch matched event.
pub async fn publish_watch_matched(nats: &NatsState, event: &nats_jobs::WatchMatchedEvent) {
    if !nats.is_available() {
        return;
    }

    let payload = match serde_json::to_value(event) {
        Ok(p) => p,
        Err(e) => {
            error!("Failed to serialize watch matched event: {}", e);
            return;
        }
    };

    nats.publisher()
        .publish_event("download.watch_matched", payload)
        .await;
}

/// Publish ingest-ready event via NATS so kobbler can process files asynchronously.
/// Returns true if the event was published successfully, false otherwise.
pub async fn publish_ingest_ready(
    nats: &NatsState,
    pool: &sqlx::PgPool,
    download: &crate::repositories::downloads::PendingDownload,
    files: &[crate::download_clients::ScannedFile],
    release_interpretation: Option<&crate::parsing::ParsedRelease>,
) -> bool {
    if !nats.is_available() {
        return false;
    }

    // Get or create idempotency key
    let idempotency_key = match crate::repositories::downloads::get_or_create_idempotency_key(
        pool,
        download.id,
    )
    .await
    {
        Ok(key) => Some(key),
        Err(e) => {
            warn!(download_id = %download.id, error = %e, "Failed to get/create idempotency key for NATS publish");
            None
        }
    };

    let payload = crate::jobs::handlers::build_callback_from_enriched(
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

    let payload_json = match serde_json::to_value(&payload) {
        Ok(p) => p,
        Err(e) => {
            error!("Failed to serialize ingest ready payload: {}", e);
            return false;
        }
    };

    match nats
        .publisher()
        .publish_event("ingest.ready", payload_json)
        .await
    {
        Some(_) => {
            info!(download_id = %download.id, "Published ingest.ready event via NATS");
            // Mark callback as sent since NATS delivery succeeded
            let _ = crate::repositories::downloads::mark_callback_sent(pool, download.id).await;
            true
        }
        None => {
            warn!(download_id = %download.id, "Failed to publish ingest.ready event via NATS");
            false
        }
    }
}

/// Subscribe to ingest-result events from kobbler.
///
/// When kobbler finishes (or fails) processing an ingest, it publishes
/// `events.ingest.result` with an `IngestResultEvent`. This subscriber
/// handles cleanup: confirming imports or triggering re-search on failure.
pub async fn subscribe_ingest_result_events(nats: NatsClient, state: crate::state::AppState) {
    use futures::StreamExt;
    use nats_jobs::{Event, IngestResultEvent, IngestResultStatus};

    let mut reconnect_rx = nats.subscribe_reconnect();
    let mut backoff = std::time::Duration::from_secs(1);

    let consumer_config = async_nats::jetstream::consumer::pull::Config {
        durable_name: Some("kobload-ingest-result".to_string()),
        filter_subject: "events.ingest.result".to_string(),
        ..Default::default()
    };

    'reconnect: loop {
        let stream = match nats.jetstream().get_stream("EVENTS").await {
            Ok(s) => s,
            Err(e) => {
                error!(
                    "Failed to get EVENTS stream for ingest-result: {}, retrying in {:?}",
                    e, backoff
                );
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(std::time::Duration::from_secs(30));
                continue 'reconnect;
            }
        };

        let consumer = match stream
            .get_or_create_consumer("kobload-ingest-result", consumer_config.clone())
            .await
        {
            Ok(c) => c,
            Err(e) => {
                error!(
                    "Failed to create ingest-result consumer: {}, retrying in {:?}",
                    e, backoff
                );
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(std::time::Duration::from_secs(30));
                continue 'reconnect;
            }
        };

        let mut messages = match consumer.messages().await {
            Ok(m) => m,
            Err(e) => {
                error!(
                    "Failed to get ingest-result messages stream: {}, retrying in {:?}",
                    e, backoff
                );
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(std::time::Duration::from_secs(30));
                continue 'reconnect;
            }
        };

        info!("Subscribed to ingest-result events from Kobbler");
        backoff = std::time::Duration::from_secs(1);

        loop {
            tokio::select! {
                _ = reconnect_rx.recv() => {
                    info!("Ingest-result consumer received reconnect signal, recreating consumer");
                    continue 'reconnect;
                }

                msg_result = messages.next() => {
                    match msg_result {
                        Some(Ok(msg)) => {
                            let event: Event = match serde_json::from_slice(&msg.payload) {
                                Ok(e) => e,
                                Err(e) => {
                                    error!("Failed to deserialize ingest-result event: {}", e);
                                    let _ = msg.ack().await;
                                    continue;
                                }
                            };

                            let result: IngestResultEvent = match serde_json::from_value(event.payload) {
                                Ok(r) => r,
                                Err(e) => {
                                    error!("Failed to deserialize IngestResultEvent payload: {}", e);
                                    let _ = msg.ack().await;
                                    continue;
                                }
                            };

                            info!(
                                download_id = %result.download_id,
                                status = ?result.status,
                                "Received ingest-result event"
                            );

                            match result.status {
                                IngestResultStatus::Imported => {
                                    handle_ingest_imported(&state, result.download_id).await;
                                }
                                IngestResultStatus::Failed => {
                                    handle_ingest_failed(&state, result.download_id, result.error).await;
                                }
                                IngestResultStatus::Deferred => {
                                    handle_ingest_deferred(&state, result.download_id, result.error).await;
                                }
                            }

                            let _ = msg.ack().await;
                        }
                        Some(Err(e)) => {
                            error!(
                                "Error receiving ingest-result event: {}, reconnecting",
                                e
                            );
                            tokio::time::sleep(backoff).await;
                            backoff = (backoff * 2).min(std::time::Duration::from_secs(30));
                            continue 'reconnect;
                        }
                        None => {
                            error!("Ingest-result events stream ended, reconnecting");
                            tokio::time::sleep(backoff).await;
                            backoff = (backoff * 2).min(std::time::Duration::from_secs(30));
                            continue 'reconnect;
                        }
                    }
                }
            }
        }
    }
}

/// Subscribe to review-completed events from koblem.
///
/// Phase 1: log-only. koblem publishes `events.review.completed` after
/// processing a review job. This subscriber logs the verdicts so the user
/// can validate LLM decisions before enabling automated actions (Phase 2).
pub async fn subscribe_review_result_events(nats: NatsClient, state: crate::state::AppState) {
    use futures::StreamExt;
    use koblem_client::types::ReviewCompletedEvent;
    use nats_jobs::Event;

    let _ = &state; // will be used in Phase 2

    let mut reconnect_rx = nats.subscribe_reconnect();
    let mut backoff = std::time::Duration::from_secs(1);

    let consumer_config = async_nats::jetstream::consumer::pull::Config {
        durable_name: Some("kobload-review-result".to_string()),
        filter_subject: "events.review.completed".to_string(),
        ..Default::default()
    };

    'reconnect: loop {
        let stream = match nats.jetstream().get_stream("EVENTS").await {
            Ok(s) => s,
            Err(e) => {
                error!(
                    "Failed to get EVENTS stream for review-result: {}, retrying in {:?}",
                    e, backoff
                );
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(std::time::Duration::from_secs(30));
                continue 'reconnect;
            }
        };

        let consumer = match stream
            .get_or_create_consumer("kobload-review-result", consumer_config.clone())
            .await
        {
            Ok(c) => c,
            Err(e) => {
                error!(
                    "Failed to create review-result consumer: {}, retrying in {:?}",
                    e, backoff
                );
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(std::time::Duration::from_secs(30));
                continue 'reconnect;
            }
        };

        let mut messages = match consumer.messages().await {
            Ok(m) => m,
            Err(e) => {
                error!(
                    "Failed to get review-result messages stream: {}, retrying in {:?}",
                    e, backoff
                );
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(std::time::Duration::from_secs(30));
                continue 'reconnect;
            }
        };

        info!("Subscribed to review-result events from koblem");
        backoff = std::time::Duration::from_secs(1);

        loop {
            tokio::select! {
                _ = reconnect_rx.recv() => {
                    info!("Review-result consumer received reconnect signal, recreating consumer");
                    continue 'reconnect;
                }

                msg_result = messages.next() => {
                    match msg_result {
                        Some(Ok(msg)) => {
                            let event: Event = match serde_json::from_slice(&msg.payload) {
                                Ok(e) => e,
                                Err(e) => {
                                    error!("Failed to deserialize review-result event: {}", e);
                                    let _ = msg.ack().await;
                                    continue;
                                }
                            };

                            let result: ReviewCompletedEvent = match serde_json::from_value(event.payload) {
                                Ok(r) => r,
                                Err(e) => {
                                    error!("Failed to deserialize ReviewCompletedEvent payload: {}", e);
                                    let _ = msg.ack().await;
                                    continue;
                                }
                            };

                            info!(
                                correlation_id = %result.correlation_id,
                                agreed = result.response.agreed.len(),
                                vetoed = result.response.vetoed.len(),
                                promoted = result.response.promoted.len(),
                                queue_wait_ms = ?result.queue_wait_ms,
                                llm_ms = ?result.response.llm_inference_ms,
                                notes = ?result.response.notes,
                                "Received koblem review result"
                            );

                            // Phase 1: log vetoed/promoted items for human validation
                            for delta in &result.response.vetoed {
                                warn!(
                                    correlation_id = %result.correlation_id,
                                    release = %delta.name,
                                    reason = ?delta.reason,
                                    "Koblem vetoed release (log-only, no action)"
                                );
                            }
                            for delta in &result.response.promoted {
                                info!(
                                    correlation_id = %result.correlation_id,
                                    release = %delta.name,
                                    reason = ?delta.reason,
                                    "Koblem promoted release (log-only, no action)"
                                );
                            }

                            let _ = msg.ack().await;
                        }
                        Some(Err(e)) => {
                            error!(
                                "Error receiving review-result event: {}, reconnecting",
                                e
                            );
                            tokio::time::sleep(backoff).await;
                            backoff = (backoff * 2).min(std::time::Duration::from_secs(30));
                            continue 'reconnect;
                        }
                        None => {
                            error!("Review-result events stream ended, reconnecting");
                            tokio::time::sleep(backoff).await;
                            backoff = (backoff * 2).min(std::time::Duration::from_secs(30));
                            continue 'reconnect;
                        }
                    }
                }
            }
        }
    }
}

/// Handle a successful import result from kobbler.
/// Confirms import in DB and deletes from download client.
async fn handle_ingest_imported(state: &crate::state::AppState, download_id: Uuid) {
    use crate::download_clients::select_download_client;
    use crate::helpers::{broadcast_download_updated, log_event};
    use crate::repositories::downloads as downloads_repo;
    use crate::repositories::watch as watch_repo;

    // Confirm import in DB (idempotent — returns None if already confirmed)
    let import_info = match downloads_repo::confirm_import(&state.pool, download_id).await {
        Ok(Some(info)) => info,
        Ok(None) => {
            debug!(download_id = %download_id, "Import already confirmed or download not found, skipping");
            return;
        }
        Err(e) => {
            error!(download_id = %download_id, error = %e, "Failed to confirm import via NATS");
            return;
        }
    };

    // Mark the watch match as import-confirmed (survives download deletion)
    if let Err(e) = watch_repo::mark_match_import_confirmed(&state.pool, download_id).await {
        warn!(error = %e, download_id = %download_id, "Failed to mark watch match import confirmed");
    }

    log_event(
        &state.pool,
        download_id,
        "import_confirmed",
        "Kobbler confirmed import (via NATS)",
        None,
    )
    .await;

    // Delete from download client if configured
    if let (Some(client_id), Some(external_id)) =
        (import_info.download_client_id, import_info.external_id)
        && let Ok(client) =
            select_download_client(&state.pool, Some(client_id), &import_info.protocol).await
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
                download_id,
                "remove_failed",
                "Failed to remove from download client after NATS import confirmation",
                Some(serde_json::json!({ "error": err.to_string() })),
            )
            .await;
        } else {
            log_event(
                &state.pool,
                download_id,
                "removed",
                "Removed from download client after NATS import confirmation",
                Some(serde_json::json!({ "external_id": external_id })),
            )
            .await;
        }
    }

    // Filesystem cleanup: delete the storage directory if it still exists.
    // SABnzbd's del_files flag is unreliable — this is the fallback.
    if let Some(ref cd) = import_info.client_data
        && let Some(storage) = cd.get("storage").and_then(|v| v.as_str())
        && !storage.is_empty()
    {
        let storage_path = std::path::Path::new(storage);
        let cleanup_dir = if storage_path.is_file() {
            match storage_path.parent() {
                Some(parent) => parent.to_path_buf(),
                None => {
                    tracing::warn!(
                        download_id = %download_id,
                        path = storage,
                        "Cannot determine parent directory of storage file"
                    );
                    return;
                }
            }
        } else {
            storage_path.to_path_buf()
        };
        let depth = cleanup_dir.components().count();
        if depth < 3 {
            tracing::warn!(
                download_id = %download_id,
                path = %cleanup_dir.display(),
                depth,
                "Refusing to delete storage path: too shallow (need at least 3 components)"
            );
        } else if cleanup_dir.exists() {
            if let Err(err) = tokio::fs::remove_dir_all(&cleanup_dir).await {
                tracing::warn!(
                    download_id = %download_id,
                    path = %cleanup_dir.display(),
                    error = %err,
                    "Failed to clean up storage directory after NATS import"
                );
            } else {
                tracing::info!(
                    download_id = %download_id,
                    path = %cleanup_dir.display(),
                    "Cleaned up storage directory after NATS import"
                );
            }
        }
    }

    broadcast_download_updated(&state.pool, &state.download_events, download_id).await;
}

/// Handle a failed import result from kobbler.
/// Marks download as failed, logs the failure, and triggers re-search.
///
/// If the download has already been confirmed as imported, the failure is
/// ignored — this happens when NATS redelivers a message and the second
/// execution fails because files were already cleaned up.
async fn handle_ingest_failed(
    state: &crate::state::AppState,
    download_id: Uuid,
    error_msg: Option<String>,
) {
    use crate::helpers::log_event;
    use crate::repositories::downloads as downloads_repo;

    // Guard: don't overwrite a successful import with a late-arriving failure
    match downloads_repo::is_import_confirmed(&state.pool, download_id).await {
        Ok(true) => {
            warn!(
                download_id = %download_id,
                error = ?error_msg,
                "Ignoring import_failed for already-confirmed download (likely NATS redelivery race)"
            );
            return;
        }
        Err(e) => {
            warn!(
                download_id = %download_id,
                error = %e,
                "Failed to check import confirmation, proceeding with failure handling"
            );
        }
        _ => {}
    }

    let reason = error_msg.as_deref().unwrap_or("Import rejected by Kobbler");

    // Claim the failure transition. A repeat import_failed for a download we
    // already failed must not log a second failure or fire a second re-search —
    // each extra re-search starts an independent replacement chain, and they
    // grab concurrently because none can see the others' in-flight download.
    match downloads_repo::claim_failure(&state.pool, download_id, reason).await {
        Ok(false) => {
            warn!(
                download_id = %download_id,
                error = ?error_msg,
                "Ignoring duplicate import_failed for already-failed download"
            );
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
        "Kobbler reported import failure (via NATS)",
        error_msg
            .as_ref()
            .map(|e| serde_json::json!({ "error": e })),
    )
    .await;

    // Trigger re-search for the watch pattern
    crate::polling::trigger_research_for_download(state, download_id).await;

    crate::helpers::broadcast_download_updated(&state.pool, &state.download_events, download_id)
        .await;
}

/// Handle a deferred import result from kobbler.
/// Logs the deferral but does NOT trigger re-search — the ingest will be
/// retried automatically when the target storage becomes available.
async fn handle_ingest_deferred(
    state: &crate::state::AppState,
    download_id: Uuid,
    reason: Option<String>,
) {
    use crate::helpers::log_event;

    log_event(
        &state.pool,
        download_id,
        "import_deferred",
        "Media root temporarily unavailable — will import automatically when available",
        reason.as_ref().map(|r| serde_json::json!({ "reason": r })),
    )
    .await;

    // Do NOT trigger re-search — this is a temporary condition, not a failure
    crate::helpers::broadcast_download_updated(&state.pool, &state.download_events, download_id)
        .await;
}
