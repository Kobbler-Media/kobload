//! Job handlers for different job types.

mod backlog_search;
pub(crate) mod bitmagnet_sync;
mod cache_cleanup;
mod definition_update;
mod grab_held_match;
pub(crate) mod hash_index_bootstrap;
pub(crate) mod hash_index_import;
mod indexer_sync;
mod job_cleanup;
mod metadata_extraction;
mod refresh_indexer_caps;
mod search_pattern_now;
mod search_request;
mod trash_filter_sync;
mod upgrade_cleanup;
mod want_request;
mod watch_directory_notify;
mod watch_directory_scan;
mod watch_directory_scan_check;
mod watch_pattern;

pub use backlog_search::BacklogSearchHandler;
pub use backlog_search::{SearchGates, backlog_search_pattern};
pub use bitmagnet_sync::BitmagnetSyncHandler;
pub use cache_cleanup::CacheCleanupHandler;
pub use definition_update::DefinitionUpdateHandler;
pub use grab_held_match::GrabHeldMatchHandler;
pub(crate) use grab_held_match::grab_held_match_core;
pub use hash_index_bootstrap::HashIndexBootstrapHandler;
pub use hash_index_import::HashIndexImportHandler;
pub use indexer_sync::IndexerSyncHandler;
pub use job_cleanup::JobCleanupHandler;
pub use metadata_extraction::{
    MetadataExtractionHandler, build_callback_from_enriched, build_scoring_context,
    release_info_from_payload,
};
pub use refresh_indexer_caps::RefreshIndexerCapsHandler;
pub use search_pattern_now::SearchPatternNowHandler;
pub(crate) use search_pattern_now::search_pattern_now_core;
pub use search_request::SearchRequestHandler;
pub use trash_filter_sync::TrashFilterSyncHandler;
pub use upgrade_cleanup::UpgradeCleanupHandler;
pub use want_request::WantRequestHandler;
pub use watch_directory_notify::WatchDirectoryNotifyHandler;
pub use watch_directory_scan::WatchDirectoryScanHandler;
pub use watch_directory_scan_check::WatchDirectoryScanCheckHandler;
pub use watch_pattern::WatchPatternDeleteHandler;
pub(crate) use watch_pattern::delete_patterns;
