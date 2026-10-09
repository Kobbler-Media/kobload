//! Decision log — instruments the search/feed pipeline with structured
//! decision tracing persisted to the database.

pub mod collector;
pub mod repository;
pub mod types;

pub use collector::DecisionCollector;
pub use types::{
    FlushPayload, LogProviderCall, LogRelease, QualityScoreBreakdown, StructuralResult,
};

/// Spawn a background task to flush a decision log payload to the database.
/// Fire-and-forget: the caller returns immediately.
pub fn spawn_flush(pool: sqlx::PgPool, payload: FlushPayload) {
    tokio::spawn(async move {
        if let Err(e) = repository::flush_session(&pool, payload).await {
            tracing::warn!("Failed to flush decision log session: {e}");
        }
    });
}
