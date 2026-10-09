//! Watch repository: watch patterns, watch matches, indexer sources.

mod matches;
mod patterns;
mod sources;

// Re-export pattern types and functions
pub use patterns::{
    OrphanPatternRow, WatchPatternInput, WatchPatternListOptions, WatchPatternListResponse,
    WatchPatternPartialUpdate, WatchPatternRow, WatchPatternSyncInput, WatchPatternUpdate,
    WatchPatternUpsertInput, append_excluded_release, bulk_delete_patterns, clear_upgrade_until,
    create_pattern, decode_cursor as decode_pattern_cursor, delete_expired_upgrades,
    delete_pattern_by_id, delete_pattern_by_source, fetch_pattern_list, find_orphaned_patterns,
    get_all_enabled_patterns, get_pattern_by_id, get_pattern_by_source,
    get_patterns_for_media_type, has_active_patterns, insert_pattern_from_sync, list_patterns,
    list_patterns_by_enabled, list_patterns_by_source, list_patterns_by_source_enabled,
    pattern_exists, reset_backlog_search, set_pattern_enabled, set_upgrade_until,
    update_pattern_by_id, update_pattern_by_source, update_pattern_full, update_pattern_partial,
    upsert_pattern_from_job,
};

// Re-export match types and functions
pub use matches::{
    PatternDownloadState, WatchMatchInput, WatchMatchListOptions, WatchMatchListResponse,
    WatchMatchRow, active_episode_keys, already_matched, batch_pattern_download_state,
    bulk_delete_matches, bulk_reject_matches, cancel_held_matches_for_pattern, create_match,
    create_match_if_not_exists, decode_cursor as decode_match_cursor, discard_hold,
    episode_has_active_download, episode_has_imported_download, fetch_match_list,
    get_best_imported_quality_for_episode, get_best_imported_quality_for_pattern,
    get_best_imported_title_for_episode, get_best_imported_title_for_pattern, get_expired_holds,
    get_held_matches_for_pattern, get_imported_episode_keys,
    get_latest_imported_download_for_episode, get_latest_imported_download_for_pattern,
    get_match_info_for_download, get_matches_pending_callback, get_watch_pattern_for_download,
    list_matches, list_matches_by_pattern, mark_match_callback_sent, mark_match_import_confirmed,
    matched_guids_for_pattern, pattern_has_active_download, pattern_has_held_match,
    pattern_has_imported_download, reclaim_skipped_match, reject_match, release_hold,
    update_match_callback_error, update_match_download_id, update_match_skipped,
};

// Re-export indexer source types and functions
pub use sources::{
    IndexerSourceInput, IndexerSourceRow, IndexerSourceUpdate, SearchContext,
    advance_all_watermarks_to_now, count_stale_caps, create_indexer_source,
    create_indexer_source_from_sync, delete_indexer_source, disable_indexer_source,
    get_indexer_source_by_id, get_sources_to_check, increment_api_calls, list_debrid_sources,
    list_enabled_sources, list_indexer_sources, list_indexer_sources_by_type,
    patterns_are_newer_than_last_rss, record_auth_failure, record_failure, record_rate_limited,
    record_success, reset_indexer_health, should_skip_backoff, should_skip_for_search,
    should_skip_source, touch_indexer_source, trigger_indexer_check,
    update_api_limits_from_response, update_indexer_caps, update_indexer_source,
    update_indexer_source_from_sync, update_indexer_source_state, update_priority,
};
