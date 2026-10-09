//! Handler for immediate pattern search triggered by user action.
//!
//! Looks up a pattern by source_system+source_id, runs backlog_search_pattern,
//! and publishes a want.response event with the result.

use crate::jobs::JobError;
use crate::jobs::worker::JobHandler;
use crate::repositories::watch as watch_repo;
use crate::state::AppState;
use async_trait::async_trait;
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use uuid::Uuid;

use super::{SearchGates, backlog_search_pattern};

pub struct SearchPatternNowHandler {
    state: AppState,
}

impl SearchPatternNowHandler {
    pub fn new(state: AppState) -> Self {
        Self { state }
    }
}

#[async_trait]
impl JobHandler for SearchPatternNowHandler {
    fn job_type(&self) -> &'static str {
        "search_pattern_now"
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

        let request: nats_jobs::SearchPatternNow = serde_json::from_value(payload)
            .map_err(|e| JobError::ExecutionFailed(e.to_string()))?;

        let found =
            search_pattern_now_core(&self.state, &request.source_system, &request.source_id)
                .await
                .map_err(JobError::ExecutionFailed)?;

        Ok(serde_json::json!({ "found": found }))
    }
}

/// Core logic shared between NATS handler and HTTP endpoint.
/// Returns Ok(true) if a match was found, Ok(false) if not.
pub(crate) async fn search_pattern_now_core(
    state: &AppState,
    source_system: &str,
    source_id: &str,
) -> Result<bool, String> {
    let pattern = watch_repo::get_pattern_by_source(&state.pool, source_system, source_id)
        .await
        .map_err(|e| format!("DB error: {e}"))?
        .ok_or_else(|| format!("Pattern not found for {source_system}/{source_id}"))?;

    info!(
        pattern_id = %pattern.id,
        source_system = %source_system,
        source_id = %source_id,
        "Running immediate search for pattern"
    );

    // Serialise against any concurrent want / RSS / backlog / research trigger
    // for the same pattern. Blocking acquire: a user-clicked "search now" must
    // not silently no-op if another session happens to hold the lock. Mirrors
    // the rejection-driven re-search in polling.rs.
    let _lock = state.pattern_search_locks.acquire(pattern.id).await;

    // search_pattern_now is user-driven ("search this now"): the user can see
    // what is already downloading, so honour the request literally.
    let found = backlog_search_pattern(state, &pattern, SearchGates::USER_REQUESTED).await;

    // Publish want.response event so Kobbler updates status
    let status = if found {
        nats_jobs::WantStatus::Downloading
    } else {
        nats_jobs::WantStatus::Watching
    };

    let response = nats_jobs::WantResponse {
        source_system: Some(source_system.to_string()),
        source_id: Some(source_id.to_string()),
        watch_pattern_id: pattern.id,
        status,
        download_id: None,
        selected_release: None,
        results_found: if found { 1 } else { 0 },
        best_score: 0.0,
        matched_pattern: None,
        matched_target: None,
        metadata: pattern.metadata.clone(),
        episodes_downloaded: None,
    };

    if let Ok(payload) = serde_json::to_value(&response) {
        let event_subject = format!("want.response.{source_system}");
        state
            .nats
            .publisher()
            .publish_event(&event_subject, payload)
            .await;
    } else {
        warn!("Failed to serialize WantResponse for search-now");
    }

    Ok(found)
}
