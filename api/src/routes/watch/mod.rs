mod matches;
mod patterns;
mod sources;

use axum::{
    Router,
    routing::{delete, get, post},
};

use crate::state::AppState;

pub fn routes() -> Router<AppState> {
    Router::new()
        // Watch patterns
        .route("/api/v1/watch", get(patterns::list_watch_patterns))
        .route("/api/v1/watch", post(patterns::create_watch_pattern))
        .route(
            "/api/v1/watch/{id}",
            axum::routing::put(patterns::update_watch_pattern).patch(patterns::patch_watch_pattern),
        )
        .route(
            "/api/v1/watch/{source_system}/{source_id}/detail",
            get(patterns::get_pattern_detail),
        )
        .route(
            "/api/v1/watch/{source_system}/{source_id}/search-now",
            post(patterns::search_pattern_now),
        )
        .route(
            "/api/v1/watch/{source_system}/{source_id}/grab-held",
            post(patterns::grab_held_match),
        )
        .route(
            "/api/v1/admin/watch/{id}/test",
            post(patterns::test_watch_pattern),
        )
        // Indexer sources
        .route(
            "/api/v1/indexer-sources",
            get(sources::list_indexer_sources),
        )
        .route(
            "/api/v1/indexer-sources",
            post(sources::create_indexer_source),
        )
        .route(
            "/api/v1/indexer-sources/sync-prowlarr",
            post(sources::sync_prowlarr_indexers),
        )
        .route(
            "/api/v1/indexer-sources/migrate/preview",
            post(sources::preview_prowlarr_migration),
        )
        .route(
            "/api/v1/indexer-sources/migrate/confirm",
            post(sources::confirm_prowlarr_migration),
        )
        .route(
            "/api/v1/indexer-sources/{id}",
            axum::routing::put(sources::update_indexer_source),
        )
        .route(
            "/api/v1/indexer-sources/{id}",
            delete(sources::delete_indexer_source),
        )
        .route(
            "/api/v1/indexer-sources/{id}/check",
            post(sources::trigger_indexer_check),
        )
        .route(
            "/api/v1/indexer-sources/{id}/reset-health",
            post(sources::reset_indexer_health),
        )
        .route("/api/v1/indexer/{id}/newznab", get(sources::proxy_newznab))
        // Watch pattern stream + bulk
        .route("/api/v1/watch/stream", get(patterns::stream_watch_patterns))
        .route(
            "/api/v1/watch/bulk-delete",
            post(patterns::bulk_delete_watch_patterns),
        )
        // Watch matches
        .route("/api/v1/watch-matches", get(matches::list_watch_matches))
        .route(
            "/api/v1/watch-matches/stream",
            get(matches::stream_watch_matches),
        )
        .route(
            "/api/v1/watch-matches/bulk-reject",
            post(matches::bulk_reject_watch_matches),
        )
        .route(
            "/api/v1/watch-matches/bulk-delete",
            post(matches::bulk_delete_watch_matches),
        )
        .route(
            "/api/v1/watch/matches/{id}/reject",
            post(matches::reject_watch_match),
        )
}
