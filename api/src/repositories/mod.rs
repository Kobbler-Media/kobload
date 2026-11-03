//! Repository layer for database operations.

pub mod app_settings;
pub mod auth;
pub mod cached_files;
pub mod clients;
pub mod config;
pub mod downloads;
pub mod handler_type_defaults;
pub mod hash_index;
pub mod indexer_category_mappings;
pub mod indexer_rss_cache;
pub mod jobs;
pub mod learned_aliases;
pub mod media_types;
pub mod quality_profiles;
pub mod release_sightings;
pub mod review;
pub mod scene_mappings;
pub mod search_cache;
pub mod stream_attempts;
pub mod stream_outcomes;
pub mod stream_torrents;
pub mod trash_filters;
pub mod watch;
pub mod watch_directories;
pub mod webhooks;
pub mod xem_cache;
pub mod xem_titles;

// Re-export common types (RepositoryError, Result) only.
// Use qualified imports (e.g., `repositories::downloads::...`) for module-specific items
// to avoid ambiguous glob re-exports.

/// Repository error type.
#[derive(thiserror::Error, Debug)]
pub enum RepositoryError {
    #[error("database error: {0}")]
    Database(#[from] sqlx::Error),

    #[error("not found")]
    NotFound,
}

/// Result type alias for repository operations.
pub type Result<T> = std::result::Result<T, RepositoryError>;
