pub mod cardigann;
mod mam;
pub mod newznab;
mod prowlarr;
mod registry;
mod serde_helpers;
mod torrentday;
pub mod types;

use sqlx::PgPool;
use uuid::Uuid;

use crate::repositories::watch as watch_repo;

pub use newznab::fetch_caps;
pub use registry::{DefaultIndexerResolver, IndexerResolver};
pub use types::{
    IndexerCapabilities, IndexerContext, IndexerError, IndexerHandler, IndexerSearchRequest,
    NewznabCaps, SearchType,
};

/// Persist an indexer failure under the right kind.
///
/// Call sites that already branch on 429 keep doing so for their own logging;
/// this is the catch-all arm, and exists so a credential rejection is never
/// silently folded into the transient-failure count.
pub async fn record_error(pool: &PgPool, source_id: Uuid, err: &IndexerError) {
    let _ = if err.auth_failed {
        watch_repo::record_auth_failure(pool, source_id, &err.to_string()).await
    } else if err.status == axum::http::StatusCode::TOO_MANY_REQUESTS {
        watch_repo::record_rate_limited(pool, source_id, err.retry_after_seconds).await
    } else {
        watch_repo::record_failure(pool, source_id, &err.to_string()).await
    };
}
