use axum::{
    Json, Router,
    extract::{Query, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use serde::{Deserialize, Serialize};
use tracing::{info, warn};
use uuid::Uuid;

use crate::auth::require_auth;
use crate::debrid::{DebridProvider, DebridProviderType, DebridTorrentStatus, Lane, Throttled};
use crate::services::stream::{
    StreamResolveContext, StreamSearchContext, load_provider_config, stream_resolve, stream_search,
};
use crate::state::AppState;
use nats_jobs::types::{StreamOutcomeReport, StreamResolveRequest, StreamSearchRequest};

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/stream/search", post(stream_search_handler))
        .route("/api/v1/stream/resolve", post(stream_resolve_handler))
        .route("/api/v1/stream/outcome", post(stream_outcome_handler))
        .route(
            "/api/v1/stream/resolve/status",
            get(stream_resolve_status_handler),
        )
}

async fn stream_search_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<StreamSearchRequest>,
) -> Result<impl axum::response::IntoResponse, (StatusCode, String)> {
    if require_auth(&state, &headers).await.is_err() {
        return Err((StatusCode::UNAUTHORIZED, "Unauthorized".to_string()));
    }

    info!(query = %request.query, "Stream search request");

    let ctx = StreamSearchContext {
        pool: &state.pool,
        http_client: &state.http_client,
        indexers: state.indexers.as_ref(),
        debrid_providers: state.debrid_providers.as_ref(),
        definitions: &state.definitions,
        xem_cache: state.xem_cache.as_ref(),
        scene_mappings: state.scene_mappings.as_ref(),
        trash_filters: state.trash_filters.as_ref(),
        download_events: &state.download_events,
        debrid_providers_arc: state.debrid_providers.clone(),
    };

    let response = stream_search(&ctx, &request)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(response))
}

async fn stream_resolve_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<StreamResolveRequest>,
) -> Result<impl axum::response::IntoResponse, (StatusCode, String)> {
    if require_auth(&state, &headers).await.is_err() {
        return Err((StatusCode::UNAUTHORIZED, "Unauthorized".to_string()));
    }

    info!(
        debrid_provider_id = %request.debrid_provider_id,
        info_hash = ?request.info_hash,
        "Stream resolve request"
    );

    let ctx = StreamResolveContext {
        pool: &state.pool,
        http_client: &state.http_client,
        debrid_providers: state.debrid_providers.as_ref(),
    };

    let response = stream_resolve(&ctx, &request)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(response))
}

async fn stream_outcome_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(report): Json<StreamOutcomeReport>,
) -> Result<StatusCode, (StatusCode, String)> {
    if require_auth(&state, &headers).await.is_err() {
        return Err((StatusCode::UNAUTHORIZED, "Unauthorized".to_string()));
    }
    crate::services::stream::outcomes::record_report(&state.pool, &report)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    Ok(StatusCode::NO_CONTENT)
}

// =============================================================================
// Resolve Status
// =============================================================================

#[derive(Debug, Deserialize)]
struct ResolveStatusQuery {
    provider_id: Uuid,
    torrent_id: String,
}

#[derive(Debug, Serialize)]
struct ResolveStatusResponse {
    status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    filename: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    filesize: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    progress: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
}

/// GET /api/v1/stream/resolve/status?provider_id={uuid}&torrent_id={prefixed_id}
///
/// Poll the status of an uncached torrent on a debrid provider.
/// When the torrent becomes ready, unrestricts the link and returns the stream URL.
async fn stream_resolve_status_handler(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<ResolveStatusQuery>,
) -> Result<impl axum::response::IntoResponse, (StatusCode, String)> {
    if require_auth(&state, &headers).await.is_err() {
        return Err((StatusCode::UNAUTHORIZED, "Unauthorized".to_string()));
    }

    // Parse provider-prefixed torrent ID
    let (provider_type_from_prefix, raw_torrent_id) =
        DebridProviderType::from_prefixed_id(&query.torrent_id).ok_or_else(|| {
            (
                StatusCode::BAD_REQUEST,
                format!("Invalid torrent_id format: {}", query.torrent_id),
            )
        })?;

    // Load provider config from DB
    let provider_config = load_provider_config(&state.pool, query.provider_id)
        .await
        .map_err(|e| (StatusCode::NOT_FOUND, e.to_string()))?;

    let db_provider_type =
        DebridProviderType::parse(&provider_config.provider_type).ok_or_else(|| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Unknown provider type: {}", provider_config.provider_type),
            )
        })?;

    // Verify the prefix matches the DB provider
    if provider_type_from_prefix != db_provider_type {
        return Err((
            StatusCode::BAD_REQUEST,
            "torrent_id prefix doesn't match provider".to_string(),
        ));
    }

    let api_key = &provider_config.api_key;
    let handler = Throttled::new(
        state.debrid_providers.get(&db_provider_type),
        api_key,
        Lane::user(),
    );

    // Get current torrent status
    let torrent = handler
        .get_torrent(&state.http_client, api_key, raw_torrent_id)
        .await
        .map_err(|e| {
            (
                StatusCode::BAD_GATEWAY,
                format!("Failed to get torrent status: {e}"),
            )
        })?;

    match torrent.status {
        DebridTorrentStatus::Cached | DebridTorrentStatus::Ready => {
            if torrent.links.is_empty() {
                return Ok(Json(ResolveStatusResponse {
                    status: "downloading".to_string(),
                    stream_url: None,
                    filename: None,
                    filesize: None,
                    progress: Some(0.99),
                    message: Some("Waiting for links...".to_string()),
                }));
            }

            let download_link = torrent.links.first().unwrap();

            // Unrestrict the link
            let unrestricted = handler
                .unrestrict_link(&state.http_client, api_key, download_link)
                .await
                .map_err(|e| {
                    (
                        StatusCode::BAD_GATEWAY,
                        format!("Failed to unrestrict link: {e}"),
                    )
                })?;

            Ok(Json(ResolveStatusResponse {
                status: "ready".to_string(),
                stream_url: Some(unrestricted.url),
                filename: Some(unrestricted.filename),
                filesize: Some(unrestricted.filesize),
                progress: None,
                message: None,
            }))
        }
        DebridTorrentStatus::WaitingFileSelection => {
            // File selection was lost — resolve correct file IDs from provider's file list.
            // No info_hash context here, so we match by largest file as fallback.
            let select_ids = crate::debrid::DebridFile::resolve_select_ids(&torrent.files, None);
            info!(
                torrent_id = %raw_torrent_id,
                select_ids = ?select_ids,
                "Resolve status: selecting files (WaitingFileSelection)"
            );
            if let Err(e) = handler
                .select_files(&state.http_client, api_key, raw_torrent_id, &select_ids)
                .await
            {
                warn!(error = %e, "select_files failed in status poll");
            }
            // Report as downloading so frontend keeps polling
            Ok(Json(ResolveStatusResponse {
                status: "downloading".to_string(),
                stream_url: None,
                filename: None,
                filesize: None,
                progress: Some(0.0),
                message: None,
            }))
        }
        DebridTorrentStatus::Downloading { progress } => Ok(Json(ResolveStatusResponse {
            status: "downloading".to_string(),
            stream_url: None,
            filename: None,
            filesize: None,
            progress: Some(progress),
            message: None,
        })),
        // Not recorded to debrid_availability_cache: this endpoint is keyed by
        // torrent_id and has no info_hash to record against. In practice
        // resolve_torrent catches a block before the client starts polling, so
        // this arm is the rare case where a torrent is refused mid-download.
        DebridTorrentStatus::Blocked => Ok(Json(ResolveStatusResponse {
            status: "error".to_string(),
            stream_url: None,
            filename: None,
            filesize: None,
            progress: None,
            message: Some("Real-Debrid refused this release. Try a different source.".to_string()),
        })),
        DebridTorrentStatus::Error { message } => Ok(Json(ResolveStatusResponse {
            status: "error".to_string(),
            stream_url: None,
            filename: None,
            filesize: None,
            progress: None,
            message: Some(message),
        })),
    }
}
