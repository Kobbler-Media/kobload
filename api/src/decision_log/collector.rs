//! In-memory decision collector — accumulates events during a search/feed
//! cycle, then flushes to the database in a single batch.

use std::sync::Mutex;
use time::OffsetDateTime;
use uuid::Uuid;

use super::types::{FlushPayload, LogProviderCall, LogRelease};

/// Accumulates decision trace data in-memory during a pipeline run.
///
/// All methods take `&self` (interior mutability via Mutex) so the collector
/// can be shared across the pipeline via `Option<&DecisionCollector>`.
///
/// When logging is disabled, no collector is created — `Option::None` is zero cost.
/// When logging is enabled, recording is cheap (Vec pushes behind a Mutex).
/// The DB write happens in the background after the pipeline finishes.
pub struct DecisionCollector {
    session_id: Uuid,
    trigger_type: String,
    watch_pattern_id: Option<Uuid>,
    indexer_source_id: Option<Uuid>,
    is_test: bool,
    started_at: OffsetDateTime,
    search_queries: Mutex<Vec<String>>,
    provider_calls: Mutex<Vec<LogProviderCall>>,
    releases: Mutex<Vec<LogRelease>>,
    outcome: Mutex<Option<String>>,
    download_id: Mutex<Option<Uuid>>,
    error_message: Mutex<Option<String>>,
}

impl DecisionCollector {
    /// Create a new collector for a session.
    pub fn new(
        trigger_type: impl Into<String>,
        watch_pattern_id: Option<Uuid>,
        indexer_source_id: Option<Uuid>,
        is_test: bool,
    ) -> Self {
        Self {
            session_id: Uuid::new_v4(),
            trigger_type: trigger_type.into(),
            watch_pattern_id,
            indexer_source_id,
            is_test,
            started_at: OffsetDateTime::now_utc(),
            search_queries: Mutex::new(Vec::new()),
            provider_calls: Mutex::new(Vec::new()),
            releases: Mutex::new(Vec::new()),
            outcome: Mutex::new(None),
            download_id: Mutex::new(None),
            error_message: Mutex::new(None),
        }
    }

    pub fn session_id(&self) -> Uuid {
        self.session_id
    }

    /// Record a search query string.
    pub fn record_query(&self, query: impl Into<String>) {
        if let Ok(mut queries) = self.search_queries.lock() {
            queries.push(query.into());
        }
    }

    /// Record a provider call. Returns the call's UUID for linking releases.
    pub fn record_provider_call(&self, call: LogProviderCall) -> Uuid {
        let id = call.id;
        if let Ok(mut calls) = self.provider_calls.lock() {
            calls.push(call);
        }
        id
    }

    /// Update an existing provider call's results after it completes.
    pub fn update_provider_call(
        &self,
        call_id: Uuid,
        results_count: i32,
        results_after_dedup: i32,
        status: &str,
        error_message: Option<String>,
        duration_ms: Option<i32>,
    ) {
        if let Ok(mut calls) = self.provider_calls.lock()
            && let Some(call) = calls.iter_mut().find(|c| c.id == call_id)
        {
            call.results_count = results_count;
            call.results_after_dedup = results_after_dedup;
            call.status = status.to_string();
            call.error_message = error_message;
            call.duration_ms = duration_ms;
        }
    }

    /// Record a release evaluation.
    pub fn record_release(&self, release: LogRelease) {
        if let Ok(mut releases) = self.releases.lock() {
            releases.push(release);
        }
    }

    /// Mark a previously recorded release as the best/winner.
    pub fn mark_best(&self, release_guid: &str) {
        if let Ok(mut releases) = self.releases.lock()
            && let Some(r) = releases
                .iter_mut()
                .rev()
                .find(|r| r.release_guid.as_deref().is_some_and(|g| g == release_guid))
        {
            r.is_best = true;
            r.decision = "best".to_string();
        }
    }

    /// Mark best by title (fallback when GUID unavailable).
    pub fn mark_best_by_title(&self, title: &str) {
        if let Ok(mut releases) = self.releases.lock()
            && let Some(r) = releases.iter_mut().rev().find(|r| r.release_title == title)
        {
            r.is_best = true;
            r.decision = "best".to_string();
        }
    }

    /// Set the session outcome.
    pub fn set_outcome(&self, outcome: impl Into<String>) {
        if let Ok(mut o) = self.outcome.lock() {
            *o = Some(outcome.into());
        }
    }

    /// Set the download ID (when a download was created).
    pub fn set_download_id(&self, id: Uuid) {
        if let Ok(mut d) = self.download_id.lock() {
            *d = Some(id);
        }
    }

    /// Set error message.
    pub fn set_error(&self, msg: impl Into<String>) {
        if let Ok(mut e) = self.error_message.lock() {
            *e = Some(msg.into());
        }
    }

    /// Consume the collector into an owned flush payload for background DB write.
    pub fn into_flush_payload(self) -> FlushPayload {
        let now = OffsetDateTime::now_utc();
        let duration_ms = (now - self.started_at).whole_milliseconds() as i32;
        let queries = self.search_queries.into_inner().unwrap_or_default();
        let provider_calls = self.provider_calls.into_inner().unwrap_or_default();
        let releases = self.releases.into_inner().unwrap_or_default();
        let releases_evaluated = releases.len() as i32;

        FlushPayload {
            session_id: self.session_id,
            trigger_type: self.trigger_type,
            watch_pattern_id: self.watch_pattern_id,
            indexer_source_id: self.indexer_source_id,
            is_test: self.is_test,
            outcome: self.outcome.into_inner().unwrap_or_default(),
            download_id: self.download_id.into_inner().unwrap_or_default(),
            releases_evaluated,
            search_queries: if queries.is_empty() {
                None
            } else {
                Some(serde_json::to_value(&queries).unwrap_or_default())
            },
            error_message: self.error_message.into_inner().unwrap_or_default(),
            started_at: self.started_at,
            finished_at: Some(now),
            duration_ms: Some(duration_ms),
            provider_calls,
            releases,
        }
    }
}
