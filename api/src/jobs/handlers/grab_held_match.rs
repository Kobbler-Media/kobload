//! Handler for grabbing a held match triggered by user action.
//!
//! Looks up a pattern by source_system+source_id, finds the held match,
//! and calls grab_held_release to create the download.

use crate::jobs::JobError;
use crate::jobs::worker::JobHandler;
use crate::repositories::watch as watch_repo;
use crate::state::AppState;
use crate::watch::holds;
use async_trait::async_trait;
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use tracing::info;
use uuid::Uuid;

pub struct GrabHeldMatchHandler {
    state: AppState,
}

impl GrabHeldMatchHandler {
    pub fn new(state: AppState) -> Self {
        Self { state }
    }
}

#[async_trait]
impl JobHandler for GrabHeldMatchHandler {
    fn job_type(&self) -> &'static str {
        "grab_held_match"
    }

    fn max_concurrent(&self) -> usize {
        2
    }

    async fn execute(
        &self,
        _job_id: Uuid,
        payload: Value,
        cancel: CancellationToken,
    ) -> Result<Value, JobError> {
        nats_jobs::check_cancelled(&cancel)?;

        let request: nats_jobs::GrabHeldMatch = serde_json::from_value(payload)
            .map_err(|e| JobError::ExecutionFailed(e.to_string()))?;

        let download_id =
            grab_held_match_core(&self.state, &request.source_system, &request.source_id)
                .await
                .map_err(JobError::ExecutionFailed)?;

        Ok(serde_json::json!({ "download_id": download_id }))
    }
}

/// Core logic shared between NATS handler and HTTP endpoint.
/// Returns Ok(Some(download_id)) if a held match was grabbed, Ok(None) if none found.
pub(crate) async fn grab_held_match_core(
    state: &AppState,
    source_system: &str,
    source_id: &str,
) -> Result<Option<Uuid>, String> {
    let pattern = watch_repo::get_pattern_by_source(&state.pool, source_system, source_id)
        .await
        .map_err(|e| format!("DB error: {e}"))?
        .ok_or_else(|| format!("Pattern not found for {source_system}/{source_id}"))?;

    let held_matches = watch_repo::get_held_matches_for_pattern(&state.pool, pattern.id)
        .await
        .map_err(|e| format!("DB error: {e}"))?;

    let held_match = match held_matches.first() {
        Some(m) => m,
        None => return Err("No held match found".to_string()),
    };

    info!(
        pattern_id = %pattern.id,
        match_id = %held_match.id,
        source_system = %source_system,
        source_id = %source_id,
        "Grabbing held match by user request"
    );

    holds::grab_held_release(state, held_match, &pattern).await;

    // Check if a download was created by looking at the match row after the grab
    let download_id = if let Ok(matches) =
        watch_repo::list_matches_by_pattern(&state.pool, pattern.id, 10).await
    {
        matches
            .iter()
            .find(|m| m.id == held_match.id)
            .and_then(|m| m.download_id)
    } else {
        None
    };

    Ok(download_id)
}
