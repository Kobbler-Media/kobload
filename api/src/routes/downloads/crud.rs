use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use tracing::{info, warn};
use uuid::Uuid;

use crate::auth::{get_api_key_id, require_auth};
use crate::download_clients::select_download_client;
use crate::helpers::{
    download_url_context_for_indexer_source, extract_indexer_source_id, log_event,
};
use crate::repositories::downloads as downloads_repo;
use crate::services::downloads::{
    DownloadServiceContext, DownloadSubmission, DownloadUrlContext, submit_download,
};
use crate::state::AppState;
use crate::types::{DownloadRequest, DownloadResponse};

use super::DeleteDownloadQuery;

/// A grab made from a pattern's manual search names the pattern, not the kind;
/// the release has to be parsed as what the pattern wants.
async fn media_kind_of_grabbed_want(
    state: &AppState,
    metadata: &serde_json::Value,
) -> Option<String> {
    let want = match metadata
        .get("pattern_id")
        .and_then(|v| v.as_str())
        .and_then(|id| id.parse::<Uuid>().ok())
    {
        Some(pattern_id) => crate::repositories::watch::get_pattern_by_id(&state.pool, pattern_id)
            .await
            .ok()
            .flatten()
            .and_then(|p| p.media_want),
        None => metadata.get("media_want").cloned(),
    }?;
    let want = serde_json::from_value::<nats_jobs::MediaWant>(want).ok()?;
    Some(crate::media::parse_media_kind(&want).to_string())
}

pub async fn create_download(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(payload): Json<DownloadRequest>,
) -> impl IntoResponse {
    info!(
        name = %payload.name,
        protocol = %payload.protocol,
        source = %payload.source,
        "Received create_download request"
    );

    if require_auth(&state, &headers).await.is_err() {
        info!("create_download: unauthorized");
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let protocol = payload.protocol.to_lowercase();
    let mut download_url = payload.download_url.clone();
    // Prefer magnet URL from release payload when available
    if let Some(release) = payload.release.as_ref()
        && let Some(magnet) = release
            .get("magnetUrl")
            .or_else(|| release.get("magnet_url"))
            .and_then(|value| value.as_str())
            .filter(|value| !value.trim().is_empty())
    {
        download_url = magnet.trim().to_string();
    }

    let mut url_context = DownloadUrlContext::default();
    let mut resolved_source_id = None;
    if let Some(release) = payload.release.as_ref()
        && let Some(indexer_source_id) = extract_indexer_source_id(release)
    {
        resolved_source_id = Some(indexer_source_id);
        match download_url_context_for_indexer_source(&state.pool, Some(indexer_source_id)).await {
            Ok(context) => url_context = context,
            Err(err) => return (StatusCode::BAD_REQUEST, err).into_response(),
        }
    }

    // Try to get the API key ID for tracking purposes
    let api_key_id = get_api_key_id(&state, &headers).await;

    let media_kind = match payload.media_kind.clone() {
        Some(kind) => Some(kind),
        None => media_kind_of_grabbed_want(&state, &payload.metadata).await,
    };

    let input = DownloadSubmission {
        source: payload.source.clone(),
        protocol: protocol.clone(),
        name: payload.name.clone(),
        download_url: download_url.clone(),
        download_client_id: payload.download_client_id,
        callback_url: Some(payload.callback_url.clone()),
        auth_token: payload.auth_token.clone(),
        metadata: payload.metadata.clone(),
        release_payload: payload.release.clone(),
        api_key_id,
        url_context,
        indexer_source_id: resolved_source_id,
        quality_profile_id: payload.quality_profile_id,
        media_kind,
        skip_duplicate_url: false,
    };

    let ctx = DownloadServiceContext {
        pool: &state.pool,
        http_client: &state.http_client,
        download_events: &state.download_events,
        download_clients: state.download_clients.as_ref(),
        indexers: state.indexers.as_ref(),
    };
    let result = submit_download(&ctx, input).await;

    match result {
        Ok(outcome) => {
            // Log manual download in decision log (fire-and-forget)
            let collector =
                crate::decision_log::DecisionCollector::new("manual", None, None, false);
            collector.set_outcome("downloaded");
            collector.set_download_id(outcome.id);
            crate::decision_log::spawn_flush(state.pool.clone(), collector.into_flush_payload());

            Json(DownloadResponse {
                id: outcome.id,
                status: outcome.status,
            })
            .into_response()
        }
        Err(err) => (err.status, err.message).into_response(),
    }
}

pub async fn get_download(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    match downloads_repo::get_download_detail(&state.pool, id).await {
        Ok(Some(item)) => Json(item).into_response(),
        Ok(None) => (StatusCode::NOT_FOUND, "not found").into_response(),
        Err(err) => {
            warn!(error = %err, "Failed to fetch download");
            (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response()
        }
    }
}

/// Query parameters for listing downloads.
#[derive(Debug, serde::Deserialize)]
pub struct ListDownloadsQuery {
    /// Number of items to return (default: 20, max: 100)
    #[serde(default = "default_limit")]
    pub limit: i64,
    /// Pagination cursor from previous response
    pub cursor: Option<String>,
    /// Filter by status (comma-separated: pending,downloading,completed,failed,imported)
    pub status: Option<String>,
    /// Text search filter (matches against name)
    pub search: Option<String>,
}

fn default_limit() -> i64 {
    20
}

/// Response for paginated download list.
#[derive(Debug, serde::Serialize)]
pub struct ListDownloadsResponse {
    pub items: Vec<crate::types::DownloadListItem>,
    pub next_cursor: Option<String>,
}

pub async fn list_downloads(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ListDownloadsQuery>,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    let status_filter = query.status.map(|s| {
        s.split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
    });

    let options = downloads_repo::DownloadListOptions {
        limit: query.limit,
        cursor: query.cursor,
        status: status_filter,
        search: query.search,
    };

    match downloads_repo::fetch_download_list(&state.pool, options).await {
        Ok(response) => Json(ListDownloadsResponse {
            items: response.items,
            next_cursor: response.next_cursor,
        })
        .into_response(),
        Err(err) => {
            warn!(error = %err, "Failed to list downloads");
            (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response()
        }
    }
}

pub async fn delete_download(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Query(query): Query<DeleteDownloadQuery>,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    // Get download info including the download client
    let deletion_info = match downloads_repo::get_deletion_info(&state.pool, id).await {
        Ok(Some(info)) => info,
        Ok(None) => return (StatusCode::NOT_FOUND, "not found").into_response(),
        Err(err) => {
            warn!(error = %err, "Failed to fetch download for deletion");
            return (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response();
        }
    };

    let (client_id, external_id, protocol, status) = (
        deletion_info.download_client_id,
        deletion_info.external_id,
        deletion_info.protocol,
        deletion_info.status,
    );

    // If cancel=true and we have client info, remove from download client.
    // For torrents, only cancel active (queued/downloading) downloads to preserve seeding.
    // For usenet, always remove since there's no seeding requirement.
    let is_active = status == "queued" || status == "downloading";
    let should_remove = query.cancel && (is_active || protocol == "usenet");
    if should_remove
        && let (Some(client_id), Some(external_id)) = (client_id, external_id.clone())
        && let Ok(client) = select_download_client(&state.pool, Some(client_id), &protocol).await
    {
        let remove_result = match state.download_clients.get(client.client_type.as_str()) {
            Some(handler) => {
                handler
                    .remove(&state.http_client, &client, &external_id, true)
                    .await
            }
            None => Ok(()),
        };
        if let Err(err) = remove_result {
            warn!(error = %err, "Failed to remove download from client");
            // Log the error but continue with deletion
            log_event(
                &state.pool,
                id,
                "remove_failed",
                "Failed to remove from download client",
                Some(serde_json::json!({ "error": err.to_string() })),
            )
            .await;
        } else {
            log_event(
                &state.pool,
                id,
                "cancelled",
                "Download cancelled and removed from client",
                Some(serde_json::json!({ "external_id": external_id })),
            )
            .await;
        }
    } else if !query.cancel {
        log_event(
            &state.pool,
            id,
            "removed",
            "Download removed from database",
            None,
        )
        .await;
    }

    // Soft-delete from database (hides from UI, keeps for watch system)
    match downloads_repo::delete_download(&state.pool, id).await {
        Ok(true) => StatusCode::NO_CONTENT.into_response(),
        Ok(false) => StatusCode::NOT_FOUND.into_response(),
        Err(err) => {
            warn!(error = %err, "Failed to delete download");
            (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response()
        }
    }
}

#[derive(Debug, serde::Deserialize)]
pub struct ClearDownloadsQuery {
    /// Comma-separated list of protocols to also remove from download client (e.g. "usenet,torrent")
    #[serde(default)]
    pub cancel_protocols: Option<String>,
}

#[derive(Debug, serde::Serialize)]
pub struct ClearDownloadsResponse {
    pub deleted: u64,
}

pub async fn clear_downloads(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ClearDownloadsQuery>,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    // If cancel_protocols specified, remove matching downloads from their clients first
    if let Some(ref protocols) = query.cancel_protocols {
        let cancel_set: std::collections::HashSet<&str> =
            protocols.split(',').map(|s| s.trim()).collect();

        if let Ok(inactive) = downloads_repo::get_inactive_deletion_info(&state.pool).await {
            for dl in inactive {
                if !cancel_set.contains(dl.protocol.as_str()) {
                    continue;
                }
                if let (Some(client_id), Some(external_id)) =
                    (dl.download_client_id, dl.external_id)
                    && let Ok(client) =
                        select_download_client(&state.pool, Some(client_id), &dl.protocol).await
                    && let Some(handler) = state.download_clients.get(client.client_type.as_str())
                    && let Err(err) = handler
                        .remove(&state.http_client, &client, &external_id, true)
                        .await
                {
                    warn!(download_id = %dl.id, error = %err, "Failed to remove from client during clear");
                }
            }
        }
    }

    match downloads_repo::clear_downloads(&state.pool).await {
        Ok(deleted) => Json(ClearDownloadsResponse { deleted }).into_response(),
        Err(err) => {
            warn!(error = %err, "Failed to clear downloads");
            (StatusCode::INTERNAL_SERVER_ERROR, "database error").into_response()
        }
    }
}

// release payload handling moved to services::downloads
