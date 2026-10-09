//! Handlers for watch pattern jobs from Kobbler.
//!
//! These jobs handle syncing watch patterns from Kobbler to Kobload.
//! Patterns are identified by (source_system, source_id) - Kobload manages internal IDs.

use crate::jobs::JobError;
use crate::jobs::worker::JobHandler;
use crate::media::media_type_key_from_media_want;
use crate::repositories::downloads as downloads_repo;
use crate::repositories::media_types;
use crate::repositories::watch as watch_repo;
use crate::state::AppState;
use async_trait::async_trait;
use nats_jobs::{WatchPatternDelete, WatchPatternUpsert};
use serde_json::Value;
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;
use tracing::info;
use uuid::Uuid;

/// Handler for watch pattern upsert jobs.
pub struct WatchPatternUpsertHandler {
    pool: PgPool,
}

impl WatchPatternUpsertHandler {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl JobHandler for WatchPatternUpsertHandler {
    fn job_type(&self) -> &'static str {
        "watch_pattern_upsert"
    }

    /// Only 1 at a time to avoid race conditions on upsert.
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

        let pattern: WatchPatternUpsert = serde_json::from_value(payload)?;

        info!(
            source_system = %pattern.source_system,
            source_id = %pattern.source_id,
            patterns_count = pattern.patterns.len(),
            match_targets_count = pattern.match_targets.len(),
            "Upserting watch pattern"
        );

        // Convert to JSON for storage
        let patterns_json = serde_json::to_value(&pattern.patterns)?;
        let match_targets_json = if pattern.match_targets.is_empty() {
            serde_json::json!([])
        } else {
            serde_json::to_value(&pattern.match_targets)?
        };
        let media_want_json = pattern
            .media_want
            .as_ref()
            .and_then(|v| serde_json::to_value(v).ok());

        let media_type_id = if let Some(ref mw) = pattern.media_want {
            let key = media_type_key_from_media_want(mw);
            media_types::get_by_key(&self.pool, key)
                .await
                .ok()
                .flatten()
                .map(|mt| mt.id)
        } else {
            None
        };

        let input = watch_repo::WatchPatternUpsertInput {
            source_system: pattern.source_system.clone(),
            source_id: pattern.source_id.clone(),
            callback_url: pattern.callback_url,
            patterns: patterns_json,
            media_want: media_want_json,
            match_targets: match_targets_json,
            match_mode: pattern.match_mode,
            min_score: pattern.min_score,
            media_type_id,
            auto_download: pattern.auto_download,
            metadata: pattern.metadata,
            enabled: pattern.enabled,
            upgrade_allowed: true,
            search_not_before: None,
        };

        watch_repo::upsert_pattern_from_job(&self.pool, input).await?;

        info!(
            source_system = %pattern.source_system,
            source_id = %pattern.source_id,
            "Watch pattern upserted"
        );

        Ok(serde_json::json!({
            "source_system": pattern.source_system,
            "source_id": pattern.source_id,
            "action": "upserted"
        }))
    }
}

/// Handler for watch pattern delete jobs.
pub struct WatchPatternDeleteHandler {
    state: AppState,
}

impl WatchPatternDeleteHandler {
    pub fn new(state: AppState) -> Self {
        Self { state }
    }
}

#[async_trait]
impl JobHandler for WatchPatternDeleteHandler {
    fn job_type(&self) -> &'static str {
        "watch_pattern_delete"
    }

    /// Only 1 at a time.
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

        let delete: WatchPatternDelete = serde_json::from_value(payload)?;

        info!(
            source_system = %delete.source_system,
            source_id = %delete.source_id,
            "Deleting watch pattern"
        );

        // Fetch pattern before deleting so we can collect download IDs and metadata
        let pattern = watch_repo::get_pattern_by_source(
            &self.state.pool,
            &delete.source_system,
            &delete.source_id,
        )
        .await
        .ok()
        .flatten();
        let metadata = pattern.as_ref().and_then(|p| p.metadata.clone());

        // Collect download IDs before cascade-deleting matches
        let download_ids = if let Some(ref p) = pattern {
            downloads_repo::get_download_ids_for_pattern(&self.state.pool, p.id)
                .await
                .unwrap_or_default()
        } else {
            vec![]
        };

        let deleted = watch_repo::delete_pattern_by_source(
            &self.state.pool,
            &delete.source_system,
            &delete.source_id,
        )
        .await?;

        if deleted {
            // Clean up orphaned hidden downloads
            let _ = downloads_repo::hard_delete_orphaned_hidden_downloads(
                &self.state.pool,
                &download_ids,
            )
            .await;
            publish_watch_pattern_deleted(
                &self.state,
                &delete.source_system,
                &delete.source_id,
                metadata,
            )
            .await;
        }

        info!(
            source_system = %delete.source_system,
            source_id = %delete.source_id,
            deleted = deleted,
            "Watch pattern delete completed"
        );

        Ok(serde_json::json!({
            "source_system": delete.source_system,
            "source_id": delete.source_id,
            "deleted": deleted
        }))
    }
}

/// Publish a watch_pattern.deleted event so subscribers (e.g., Kobbler) can reset entity statuses.
pub(crate) async fn publish_watch_pattern_deleted(
    state: &AppState,
    source_system: &str,
    source_id: &str,
    metadata: Option<serde_json::Value>,
) {
    let event = nats_jobs::WatchPatternDeletedEvent {
        source_system: source_system.to_string(),
        source_id: source_id.to_string(),
        metadata,
    };
    if let Ok(payload) = serde_json::to_value(&event) {
        state
            .nats
            .publisher()
            .publish_event("watch_pattern.deleted", payload)
            .await;
    }
}
