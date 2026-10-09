use sqlx::PgPool;
use tokio::sync::broadcast;
use uuid::Uuid;

use crate::repositories::downloads as downloads_repo;
use crate::types::{DownloadListItem, DownloadStreamEvent};

/// Log a download event for lifecycle tracking
pub async fn log_event(
    pool: &PgPool,
    download_id: Uuid,
    event_type: &str,
    message: &str,
    details: Option<serde_json::Value>,
) {
    let _ = downloads_repo::log_event(pool, download_id, event_type, message, details).await;
}

/// Fetch a single download by ID and return it as a DownloadListItem
pub async fn fetch_download_by_id(
    pool: &PgPool,
    id: Uuid,
) -> Result<Option<DownloadListItem>, sqlx::Error> {
    downloads_repo::fetch_download_by_id(pool, id)
        .await
        .map_err(|e| match e {
            crate::repositories::RepositoryError::Database(e) => e,
            _ => sqlx::Error::RowNotFound,
        })
}

/// Broadcast a download created event
pub async fn broadcast_download_created(
    pool: &PgPool,
    tx: &broadcast::Sender<DownloadStreamEvent>,
    download_id: Uuid,
) {
    if let Ok(Some(download)) = fetch_download_by_id(pool, download_id).await {
        let _ = tx.send(DownloadStreamEvent::Created { download });
    }
}

/// Broadcast a download updated event
pub async fn broadcast_download_updated(
    pool: &PgPool,
    tx: &broadcast::Sender<DownloadStreamEvent>,
    download_id: Uuid,
) {
    if let Ok(Some(download)) = fetch_download_by_id(pool, download_id).await {
        let _ = tx.send(DownloadStreamEvent::Updated { download });
    }
}
