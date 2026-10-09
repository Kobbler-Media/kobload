//! Handler for want_request jobs from Kobbler.
//!
//! This job processes want requests: immediate search + watch pattern creation,
//! then publishes a response event back to the source system.

use std::sync::Arc;

use crate::download_clients::DownloadClientResolver;
use crate::indexers::IndexerResolver;
use crate::jobs::JobError;
use crate::jobs::worker::JobHandler;
use crate::media::media_type_key_from_media_want;
use crate::nats::NatsState;
use crate::repositories::media_types;
use crate::scene_mappings::SceneMappingCache;
use crate::services::want::{WantServiceContext, WantServiceRequest, handle_want};
use crate::trash_filters::TrashFilterCache;
use crate::watch::pattern_lock::PatternSearchLocks;
use crate::xem::XemCache;
use async_trait::async_trait;
use nats_jobs::{WantRequest, WantResponse, WantStatus};
use serde_json::Value;
use sqlx::PgPool;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use uuid::Uuid;

/// Handler for want request jobs.
pub struct WantRequestHandler {
    pool: PgPool,
    nats: NatsState,
    http_client: reqwest::Client,
    download_events: broadcast::Sender<crate::types::DownloadStreamEvent>,
    download_clients: Arc<dyn DownloadClientResolver>,
    indexers: Arc<dyn IndexerResolver>,
    xem_cache: Arc<XemCache>,
    scene_mappings: Arc<SceneMappingCache>,
    trash_filters: Arc<TrashFilterCache>,
    pattern_search_locks: PatternSearchLocks,
}

impl WantRequestHandler {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        pool: PgPool,
        nats: NatsState,
        http_client: reqwest::Client,
        download_events: broadcast::Sender<crate::types::DownloadStreamEvent>,
        download_clients: Arc<dyn DownloadClientResolver>,
        indexers: Arc<dyn IndexerResolver>,
        xem_cache: Arc<XemCache>,
        scene_mappings: Arc<SceneMappingCache>,
        trash_filters: Arc<TrashFilterCache>,
        pattern_search_locks: PatternSearchLocks,
    ) -> Self {
        Self {
            pool,
            nats,
            http_client,
            download_events,
            download_clients,
            indexers,
            xem_cache,
            scene_mappings,
            trash_filters,
            pattern_search_locks,
        }
    }
}

#[async_trait]
impl JobHandler for WantRequestHandler {
    fn job_type(&self) -> &'static str {
        "want_request"
    }

    /// Allow some concurrency but not too much to avoid overwhelming Prowlarr.
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

        let request: WantRequest = serde_json::from_value(payload)?;

        info!(
            job_id = %job_id,
            source_system = %request.source_system,
            source_id = %request.source_id,
            patterns_count = request.patterns.len(),
            "Processing want_request job"
        );

        let match_targets = request.match_targets.clone();

        // Derive media_type_id from MediaWant so category filtering and
        // protocol preferences apply correctly.
        let media_type_id = if let Some(ref mw) = request.media_want {
            let key = media_type_key_from_media_want(mw);
            media_types::get_by_key(&self.pool, key)
                .await
                .ok()
                .flatten()
                .map(|mt| mt.id)
        } else {
            None
        };

        let search_not_before = request.search_not_before.as_deref().and_then(|s| {
            time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).ok()
        });

        nats_jobs::check_cancelled(&cancel)?;

        let service_request = WantServiceRequest {
            source_system: request.source_system.clone(),
            source_id: request.source_id.clone(),
            callback_url: request.callback_url.clone(),
            auth_token: request.auth_token.clone(),
            patterns: request.patterns.clone(),
            match_targets,
            match_mode: request.match_mode.clone(),
            min_score: request.min_score,
            early_exit_score: request.early_exit_score,
            media_type_id,
            auto_download: request.auto_download,
            limit: request.limit,
            metadata: request.metadata.clone(),
            media_want: request.media_want.clone(),
            upgrade_allowed: request.upgrade_allowed.unwrap_or(true),
            manual: request.manual,
            search_not_before,
            watch_pattern_id: None,
            excluded_releases: request
                .excluded_releases
                .as_ref()
                .and_then(|v| serde_json::to_value(v).ok()),
        };

        let ctx = WantServiceContext {
            pool: &self.pool,
            http_client: &self.http_client,
            download_events: &self.download_events,
            download_clients: self.download_clients.as_ref(),
            indexers: self.indexers.as_ref(),
            xem_cache: self.xem_cache.as_ref(),
            scene_mappings: self.scene_mappings.as_ref(),
            trash_filters: self.trash_filters.get_filters(),
            pattern_search_locks: &self.pattern_search_locks,
            nats: &self.nats,
        };

        let result = match handle_want(&ctx, service_request).await {
            Ok(result) => result,
            Err(e) => {
                warn!(
                    error = %e,
                    source_system = %request.source_system,
                    source_id = %request.source_id,
                    "Want request failed"
                );

                self.publish_response(WantResponse {
                    source_system: Some(request.source_system.clone()),
                    source_id: Some(request.source_id.clone()),
                    watch_pattern_id: Uuid::nil(),
                    status: WantStatus::Failed,
                    download_id: None,
                    selected_release: None,
                    results_found: 0,
                    best_score: 0.0,
                    matched_pattern: None,
                    matched_target: None,
                    metadata: request.metadata.clone(),
                    episodes_downloaded: None,
                })
                .await;

                return Err(JobError::ExecutionFailed(e.message));
            }
        };

        info!(
            watch_pattern_id = %result.watch_pattern_id,
            status = %result.status,
            download_id = ?result.download_id,
            results_found = result.results_found,
            best_score = result.best_score,
            "Want request completed"
        );

        nats_jobs::check_cancelled(&cancel)?;

        // Publish response event
        let response = WantResponse {
            source_system: Some(request.source_system.clone()),
            source_id: Some(request.source_id.clone()),
            watch_pattern_id: result.watch_pattern_id,
            status: result.status,
            download_id: result.download_id,
            selected_release: result.selected_release.map(|r| r.to_dto()),
            results_found: result.results_found,
            best_score: result.best_score,
            matched_pattern: result.matched_pattern,
            matched_target: result.matched_target,
            metadata: request.metadata.clone(),
            episodes_downloaded: if result.episodes_downloaded.is_empty() {
                None
            } else {
                Some(result.episodes_downloaded)
            },
        };

        self.publish_response(response.clone()).await;

        Ok(serde_json::json!({
            "source_system": request.source_system,
            "source_id": request.source_id,
            "watch_pattern_id": result.watch_pattern_id,
            "status": result.status.to_string(),
            "download_id": result.download_id,
            "results_found": result.results_found,
            "best_score": result.best_score,
        }))
    }
}

impl WantRequestHandler {
    async fn publish_response(&self, response: WantResponse) {
        if !self.nats.is_available() {
            return;
        }

        let payload = match serde_json::to_value(&response) {
            Ok(p) => p,
            Err(e) => {
                tracing::error!("Failed to serialize want response: {}", e);
                return;
            }
        };

        // Publish to events.want.response.{source_system}
        let source_system = response.source_system.as_deref().unwrap_or("unknown");
        let subject = format!("want.response.{}", source_system);
        self.nats.publisher().publish_event(&subject, payload).await;
    }
}
