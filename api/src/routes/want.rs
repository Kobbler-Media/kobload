use axum::{
    Json, Router,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    routing::post,
};
use serde::Serialize;
use time::{Duration, OffsetDateTime};
use tracing::{info, warn};
use uuid::Uuid;

use crate::auth::require_auth;
use crate::repositories::app_settings;
use crate::repositories::media_types as media_types_repo;
use crate::repositories::watch as watch_repo;
use crate::services::want::{WantServiceContext, WantServiceRequest, handle_want};
use crate::state::AppState;
use crate::watch::episode_key;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api/v1/want", post(create_want))
        .route("/api/v1/want/bulk", post(bulk_create_want))
        .route(
            "/api/v1/want/{source_system}/{source_id}/confirm",
            post(confirm_want),
        )
}

// ============================================================================
// Types
// ============================================================================

/// Response from bulk want creation
#[derive(Debug, Serialize)]
pub struct BulkWantResponse {
    pub results: Vec<BulkWantResult>,
    pub succeeded: usize,
    pub failed: usize,
}

/// Individual result within a bulk want response
#[derive(Debug, Serialize)]
pub struct BulkWantResult {
    /// "source_system:source_id" for correlation
    pub source_key: String,
    /// The want response if successful
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<nats_jobs::WantResponse>,
    /// Error message if failed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Response from confirming a want when upgrade window is active
#[derive(Debug, Serialize)]
struct ConfirmWantResponse {
    status: String,
    #[serde(
        with = "time::serde::rfc3339::option",
        skip_serializing_if = "Option::is_none"
    )]
    upgrade_until: Option<OffsetDateTime>,
    #[serde(skip_serializing_if = "Option::is_none")]
    episodes_imported: Option<usize>,
    #[serde(skip_serializing_if = "Option::is_none")]
    episodes_total: Option<usize>,
}

// ============================================================================
// Handlers
// ============================================================================

async fn resolve_media_type_id(pool: &sqlx::PgPool, media_type: &Option<String>) -> Option<Uuid> {
    if let Some(mt) = media_type {
        media_types_repo::get_by_key(pool, mt)
            .await
            .ok()
            .flatten()
            .map(|row| row.id)
    } else {
        None
    }
}

async fn create_want(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<nats_jobs::WantRequest>,
) -> Result<Json<nats_jobs::WantResponse>, (StatusCode, String)> {
    if require_auth(&state, &headers).await.is_err() {
        return Err((StatusCode::UNAUTHORIZED, "Unauthorized".to_string()));
    }

    // If patterns is empty but query is provided, use query as a single pattern
    let patterns = if request.patterns.is_empty() {
        match &request.query {
            Some(q) if !q.trim().is_empty() => vec![q.trim().to_string()],
            _ => {
                return Err((
                    StatusCode::BAD_REQUEST,
                    "At least one of 'patterns' or 'query' must be specified".to_string(),
                ));
            }
        }
    } else {
        request.patterns.clone()
    };

    let media_type_id = resolve_media_type_id(&state.pool, &request.media_type).await;

    info!(
        source_system = %request.source_system,
        source_id = %request.source_id,
        patterns = ?patterns,
        match_targets_count = request.match_targets.len(),
        auto_download = request.auto_download,
        "Creating want request"
    );

    let search_not_before = request.search_not_before.as_deref().and_then(|s| {
        time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).ok()
    });

    let service_request = WantServiceRequest {
        source_system: request.source_system.clone(),
        source_id: request.source_id.clone(),
        callback_url: request.callback_url.clone(),
        auth_token: request.auth_token.clone(),
        patterns,
        match_targets: request.match_targets.clone(),
        match_mode: request.match_mode.clone(),
        min_score: request.min_score,
        early_exit_score: request.early_exit_score,
        media_type_id,
        auto_download: request.auto_download,
        limit: request.limit,
        metadata: request.metadata.clone(),
        media_want: request.media_want.clone(),
        upgrade_allowed: request.upgrade_allowed.unwrap_or(true),
        manual: request.manual,
        search_not_before,
        watch_pattern_id: None,
        excluded_releases: request
            .excluded_releases
            .as_ref()
            .and_then(|v| serde_json::to_value(v).ok()),
    };

    let ctx = WantServiceContext {
        pool: &state.pool,
        http_client: &state.http_client,
        download_events: &state.download_events,
        download_clients: state.download_clients.as_ref(),
        indexers: state.indexers.as_ref(),
        xem_cache: state.xem_cache.as_ref(),
        scene_mappings: state.scene_mappings.as_ref(),
        trash_filters: state.trash_filters.get_filters(),
        pattern_search_locks: &state.pattern_search_locks,
        nats: &state.nats,
    };

    let result = handle_want(&ctx, service_request)
        .await
        .map_err(|e| (e.status, e.message))?;

    info!(
        watch_pattern_id = %result.watch_pattern_id,
        status = ?result.status,
        download_id = ?result.download_id,
        results_found = result.results_found,
        best_score = result.best_score,
        "Want request completed"
    );

    Ok(Json(nats_jobs::WantResponse {
        source_system: Some(request.source_system),
        source_id: Some(request.source_id),
        watch_pattern_id: result.watch_pattern_id,
        status: result.status,
        download_id: result.download_id,
        selected_release: result.selected_release.map(|r| r.to_dto()),
        results_found: result.results_found,
        best_score: result.best_score,
        matched_pattern: result.matched_pattern,
        matched_target: result.matched_target,
        metadata: request.metadata,
        episodes_downloaded: if result.episodes_downloaded.is_empty() {
            None
        } else {
            Some(result.episodes_downloaded)
        },
    }))
}

/// Confirm successful ingest - deletes the pattern or starts an upgrade window
async fn confirm_want(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((source_system, source_id)): Path<(String, String)>,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return (StatusCode::UNAUTHORIZED, "Unauthorized".to_string()).into_response();
    }

    info!(
        source_system = %source_system,
        source_id = %source_id,
        "Confirming want"
    );

    let pattern =
        match watch_repo::get_pattern_by_source(&state.pool, &source_system, &source_id).await {
            Ok(Some(p)) => p,
            Ok(None) => {
                info!(
                    source_system = %source_system,
                    source_id = %source_id,
                    "Watch pattern not found (already deleted)"
                );
                return StatusCode::NO_CONTENT.into_response();
            }
            Err(e) => {
                warn!(
                    error = %e,
                    source_system = %source_system,
                    source_id = %source_id,
                    "Failed to look up watch pattern"
                );
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Failed to look up watch pattern: {e}"),
                )
                    .into_response();
            }
        };

    // For TvSeason patterns, check if all episodes are covered before confirming
    if let Some(ref media_want) = pattern.media_want
        && let Some(episodes_needed) = episode_key::get_episodes_needed(media_want)
    {
        let mut covered_keys =
            match watch_repo::get_imported_episode_keys(&state.pool, pattern.id).await {
                Ok(keys) => keys,
                Err(e) => {
                    warn!(
                        error = %e,
                        source_system = %source_system,
                        source_id = %source_id,
                        "Failed to get imported episode keys"
                    );
                    return (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        format!("Failed to get imported episode keys: {e}"),
                    )
                        .into_response();
                }
            };

        // Episodes already in the library (from media_want) count as covered
        // even without a Kobload download record.
        for key in episode_key::in_library_episode_keys(&episodes_needed) {
            if !covered_keys.contains(&key) {
                covered_keys.push(key);
            }
        }

        if !episode_key::all_episodes_covered(&covered_keys, &episodes_needed) {
            info!(
                source_system = %source_system,
                source_id = %source_id,
                episodes_covered = covered_keys.len(),
                episodes_total = episodes_needed.len(),
                "TvSeason partially fulfilled, keeping pattern active"
            );
            return (
                StatusCode::OK,
                Json(ConfirmWantResponse {
                    status: "partial".to_string(),
                    upgrade_until: None,
                    episodes_imported: Some(covered_keys.len()),
                    episodes_total: Some(episodes_needed.len()),
                }),
            )
                .into_response();
        }
    }

    if !pattern.upgrade_allowed {
        // No upgrades - delete the pattern outright
        match watch_repo::delete_pattern_by_source(&state.pool, &source_system, &source_id).await {
            Ok(_) => {
                info!(
                    source_system = %source_system,
                    source_id = %source_id,
                    "Watch pattern deleted (upgrades not allowed)"
                );
                StatusCode::NO_CONTENT.into_response()
            }
            Err(e) => {
                warn!(
                    error = %e,
                    source_system = %source_system,
                    source_id = %source_id,
                    "Failed to delete watch pattern"
                );
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Failed to delete watch pattern: {e}"),
                )
                    .into_response()
            }
        }
    } else {
        // Upgrades allowed - set (or extend) the upgrade window
        let defaults = match app_settings::get_upgrade_defaults(&state.pool).await {
            Ok(d) => d,
            Err(e) => {
                warn!(error = %e, "Failed to load upgrade defaults");
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Failed to load upgrade defaults: {e}"),
                )
                    .into_response();
            }
        };

        let window_hours = defaults.upgrade_window_hours.unwrap_or(720) as i64;
        let until = OffsetDateTime::now_utc() + Duration::hours(window_hours);

        match watch_repo::set_upgrade_until(&state.pool, pattern.id, until).await {
            Ok(_) => {
                info!(
                    source_system = %source_system,
                    source_id = %source_id,
                    upgrade_until = %until,
                    window_hours,
                    "Upgrade window set"
                );
                (
                    StatusCode::OK,
                    Json(ConfirmWantResponse {
                        status: "upgrading".to_string(),
                        upgrade_until: Some(until),
                        episodes_imported: None,
                        episodes_total: None,
                    }),
                )
                    .into_response()
            }
            Err(e) => {
                warn!(
                    error = %e,
                    source_system = %source_system,
                    source_id = %source_id,
                    "Failed to set upgrade window"
                );
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    format!("Failed to set upgrade window: {e}"),
                )
                    .into_response()
            }
        }
    }
}

/// Bulk create wants - accepts an array of want requests
async fn bulk_create_want(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(requests): Json<Vec<nats_jobs::WantRequest>>,
) -> Result<Json<BulkWantResponse>, (StatusCode, String)> {
    if require_auth(&state, &headers).await.is_err() {
        return Err((StatusCode::UNAUTHORIZED, "Unauthorized".to_string()));
    }

    if requests.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "At least one want request is required".to_string(),
        ));
    }

    if requests.len() > 100 {
        return Err((
            StatusCode::BAD_REQUEST,
            "Maximum 100 wants per bulk request".to_string(),
        ));
    }

    info!(count = requests.len(), "Processing bulk want request");

    let mut results = Vec::with_capacity(requests.len());

    for request in requests {
        let source_key = format!("{}:{}", request.source_system, request.source_id);

        let patterns = if request.patterns.is_empty() {
            match &request.query {
                Some(q) if !q.trim().is_empty() => vec![q.trim().to_string()],
                _ => {
                    results.push(BulkWantResult {
                        source_key,
                        result: None,
                        error: Some(
                            "At least one of 'patterns' or 'query' must be specified".to_string(),
                        ),
                    });
                    continue;
                }
            }
        } else {
            request.patterns
        };

        let media_type_id = resolve_media_type_id(&state.pool, &request.media_type).await;

        let source_system = request.source_system.clone();
        let source_id = request.source_id.clone();
        let req_metadata = request.metadata.clone();

        let search_not_before = request.search_not_before.as_deref().and_then(|s| {
            time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).ok()
        });

        let service_request = WantServiceRequest {
            source_system: request.source_system,
            source_id: request.source_id,
            callback_url: request.callback_url,
            auth_token: request.auth_token,
            patterns,
            match_targets: request.match_targets,
            match_mode: request.match_mode,
            min_score: request.min_score,
            early_exit_score: request.early_exit_score,
            media_type_id,
            auto_download: request.auto_download,
            limit: request.limit,
            metadata: request.metadata,
            media_want: request.media_want,
            upgrade_allowed: request.upgrade_allowed.unwrap_or(true),
            manual: request.manual,
            search_not_before,
            watch_pattern_id: None,
            excluded_releases: request
                .excluded_releases
                .as_ref()
                .and_then(|v| serde_json::to_value(v).ok()),
        };

        let ctx = WantServiceContext {
            pool: &state.pool,
            http_client: &state.http_client,
            download_events: &state.download_events,
            download_clients: state.download_clients.as_ref(),
            indexers: state.indexers.as_ref(),
            xem_cache: state.xem_cache.as_ref(),
            scene_mappings: state.scene_mappings.as_ref(),
            trash_filters: state.trash_filters.get_filters(),
            pattern_search_locks: &state.pattern_search_locks,
            nats: &state.nats,
        };

        match handle_want(&ctx, service_request).await {
            Ok(result) => {
                results.push(BulkWantResult {
                    source_key,
                    result: Some(nats_jobs::WantResponse {
                        source_system: Some(source_system),
                        source_id: Some(source_id),
                        watch_pattern_id: result.watch_pattern_id,
                        status: result.status,
                        download_id: result.download_id,
                        selected_release: result.selected_release.map(|r| r.to_dto()),
                        results_found: result.results_found,
                        best_score: result.best_score,
                        matched_pattern: result.matched_pattern,
                        matched_target: result.matched_target,
                        metadata: req_metadata,
                        episodes_downloaded: if result.episodes_downloaded.is_empty() {
                            None
                        } else {
                            Some(result.episodes_downloaded)
                        },
                    }),
                    error: None,
                });
            }
            Err(e) => {
                warn!(source_key = %source_key, error = %e, "Bulk want item failed");
                results.push(BulkWantResult {
                    source_key,
                    result: None,
                    error: Some(e.message),
                });
            }
        }
    }

    let succeeded = results.iter().filter(|r| r.result.is_some()).count();
    let failed = results.iter().filter(|r| r.error.is_some()).count();

    info!(succeeded, failed, "Bulk want request completed");

    Ok(Json(BulkWantResponse {
        results,
        succeeded,
        failed,
    }))
}
