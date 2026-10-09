//! Downloads repository: downloads table, lifecycle, polling state.

mod crud;
mod lifecycle;
mod polling;

use time::OffsetDateTime;
use uuid::Uuid;

// =============================================================================
// Types
// =============================================================================

/// Pending download for polling.
#[derive(Debug)]
pub struct PendingDownload {
    pub id: Uuid,
    pub download_client_id: Option<Uuid>,
    pub external_id: Option<String>,
    pub status: String,
    pub protocol: String,
    pub name: String,
    pub callback_url: String,
    pub auth_token: Option<String>,
    pub metadata: serde_json::Value,
    pub callback_sent_at: Option<OffsetDateTime>,
    pub file_path: Option<String>,
    pub error: Option<String>,
    pub callback_attempts: i32,
    pub files_scan_error: Option<String>,
    pub files_json: Option<serde_json::Value>,
    pub release_payload: Option<serde_json::Value>,
    pub media_kind: Option<String>,
    /// Idempotency key for callback deduplication
    pub callback_idempotency_key: Option<Uuid>,
}

impl PendingDownload {
    /// Whether this download has an external callback destination.
    /// Downloads initiated directly from kobload's search UI have an empty callback_url.
    pub fn has_callback(&self) -> bool {
        !self.callback_url.is_empty()
    }
}

/// Input for creating a download.
#[derive(Debug, Default)]
pub struct DownloadInput {
    pub source: String,
    pub protocol: String,
    pub profile_id: Option<Uuid>,
    pub name: String,
    pub download_url: String,
    pub download_client_id: Uuid,
    pub external_id: Option<String>,
    pub callback_url: Option<String>,
    pub auth_token: Option<String>,
    pub metadata: Option<serde_json::Value>,
    pub status: String,
    pub progress: Option<f64>,
    pub error: Option<String>,
    pub release_payload: Option<serde_json::Value>,
    pub api_key_id: Option<Uuid>,
    pub quality_profile_id: Option<Uuid>,
    pub media_kind: Option<String>,
    /// Error that occurred during submission (for pending_retry status)
    pub submission_error: Option<String>,
}

/// Input for updating download status after poll.
#[derive(Debug, Default)]
pub struct DownloadStatusUpdate<'a> {
    pub status: &'a str,
    pub file_path: Option<&'a str>,
    pub error: Option<&'a str>,
    pub progress: Option<f64>,
    pub files_scan_error: Option<&'a str>,
    pub client_data: Option<&'a serde_json::Value>,
}

// Re-export CRUD operations
pub use crud::{
    DownloadDeletionInfo, DownloadListOptions, DownloadListResponse, InactiveDeletionInfo,
    PendingRetryDownload, RecentDownloadByUrl, clear_downloads, create_download, delete_download,
    fetch_download_by_id, fetch_download_list, fetch_pending_download_by_id,
    fetch_pending_downloads, fetch_pending_retry_downloads, find_recent_active_download_by_url,
    get_deletion_info, get_download_detail, get_download_ids_for_pattern,
    get_download_ids_for_patterns, get_inactive_deletion_info,
    hard_delete_orphaned_hidden_downloads, reserve_download,
};

// Re-export polling operations
pub use polling::{
    claim_failure, clear_files_scan_error, finalize_submission, get_or_create_idempotency_key,
    mark_callback_sent, mark_failed, mark_submission_failed, mark_submission_success,
    touch_callback_attempt, update_callback_attempt, update_callback_error,
    update_enriched_metadata, update_files_json, update_files_scan_error, update_poll_status,
    update_status, update_submission_attempt,
};

// Re-export lifecycle operations
pub use lifecycle::{
    CompletionCallbackInfo, FileByIdInfo, ImportConfirmationInfo, ReingestInfo, RescanInfo,
    complete_download_status, confirm_import, get_file_by_id, get_file_path, get_reingest_info,
    get_rescan_info, is_import_confirmed, list_download_events, log_event, reset_callback_tracking,
};
