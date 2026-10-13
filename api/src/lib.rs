//! Kobload API library - exposes types and functions for testing.

pub mod archive;
pub mod auth;
pub mod backup;
pub mod clock;
pub mod cpu_bound;
pub mod debrid;
pub mod decision_log;
pub mod download_clients;
pub mod file_scoring;
pub mod format;
pub mod hash_sources;
pub mod helpers;
pub mod http;
pub mod indexers;
pub mod jobs;
pub mod media;
pub mod nats;
pub mod newznab;
pub mod openapi;
pub mod parsing;
pub mod polling;
pub mod prowlarr;
pub mod quality;
pub mod rate_limit;
pub mod repositories;
pub mod routes;
pub mod scene_mappings;
pub mod scene_titles;
pub mod services;
pub mod startup;
pub mod state;
#[cfg(test)]
pub mod test_utils;
pub mod trash_filters;
pub mod types;
pub mod watch;
pub mod webhook_dispatch;
pub mod xem;

pub use state::AppState;
