use crate::jobs::JobError;
use crate::jobs::worker::JobHandler;
use crate::repositories::RepositoryError;
use crate::repositories::downloads as downloads_repo;
use crate::repositories::watch as watch_repo;
use crate::state::AppState;
use crate::types::WatchPatternStreamEvent;
use async_trait::async_trait;
use nats_jobs::WatchPatternDelete;
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use tracing::info;
use uuid::Uuid;

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

        let id = watch_repo::get_pattern_by_source(
            &self.state.pool,
            &delete.source_system,
            &delete.source_id,
        )
        .await?
        .map(|p| p.id);
        let deleted = match id {
            Some(id) => !delete_patterns(&self.state, &[id]).await?.is_empty(),
            None => false,
        };

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

/// The one way a pattern is removed, whoever asks. The source is told so
/// Kobbler resets what it wanted.
pub(crate) async fn delete_patterns(
    state: &AppState,
    ids: &[Uuid],
) -> Result<Vec<watch_repo::DeletedPattern>, RepositoryError> {
    let download_ids = downloads_repo::get_download_ids_for_patterns(&state.pool, ids)
        .await
        .unwrap_or_default();
    let deleted = watch_repo::bulk_delete_patterns(&state.pool, ids).await?;
    let _ = downloads_repo::hard_delete_orphaned_hidden_downloads(&state.pool, &download_ids).await;
    for pattern in &deleted {
        publish_watch_pattern_deleted(
            state,
            &pattern.source_system,
            &pattern.source_id,
            pattern.metadata.clone(),
        )
        .await;
        let _ = state
            .watch_pattern_events
            .send(WatchPatternStreamEvent::Deleted { id: pattern.id });
    }
    Ok(deleted)
}

async fn publish_watch_pattern_deleted(
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
