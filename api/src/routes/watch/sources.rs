use std::collections::HashMap;

use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::auth::require_auth;
use crate::indexers::{IndexerContext, IndexerSearchRequest};
use crate::jobs::{self, JobContext};
use crate::newznab::{
    build_caps, build_search_response, normalize_limit, normalize_offset, normalize_query,
};
use crate::repositories::watch as watch_repo;
use crate::state::AppState;
use crate::watch::{
    IndexerSourceRequest, IndexerSourceResponse, prowlarr_migrate,
    prowlarr_sync::sync_prowlarr_indexers as do_sync_prowlarr,
};

/// List all indexer sources
pub(super) async fn list_indexer_sources(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<Vec<IndexerSourceResponse>>, (StatusCode, String)> {
    if require_auth(&state, &headers).await.is_err() {
        return Err((StatusCode::UNAUTHORIZED, "Unauthorized".to_string()));
    }

    let sources = watch_repo::list_indexer_sources(&state.pool)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    Ok(Json(sources.into_iter().map(Into::into).collect()))
}

/// Create a new indexer source
pub(super) async fn create_indexer_source(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<IndexerSourceRequest>,
) -> Result<Json<IndexerSourceResponse>, (StatusCode, String)> {
    if require_auth(&state, &headers).await.is_err() {
        return Err((StatusCode::UNAUTHORIZED, "Unauthorized".to_string()));
    }

    let input = watch_repo::IndexerSourceInput {
        name: request.name,
        handler_type: request.handler_type,
        handler_config: request.handler_config,
        category_ids: request.category_ids,
        media_type_ids: request.media_type_ids.unwrap_or_default(),
        enabled: request.enabled,
        priority: request.priority,
        seed_ratio: request.seed_ratio,
        seed_time_minutes: request.seed_time_minutes,
        search_mode: request.search_mode,
        auto_throttle: request.auto_throttle,
        api_limit_daily: request.api_limit_daily,
        debrid_mode: request.debrid_mode,
    };

    let handler_type = input.handler_type.clone();
    let id = watch_repo::create_indexer_source(&state.pool, input)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Auto-trigger Prowlarr sync when creating a Prowlarr source
    if handler_type == "prowlarr" {
        let pool = state.pool.clone();
        let http = state.http_client.clone();
        tokio::spawn(async move {
            if let Err(e) = do_sync_prowlarr(&pool, &http, id).await {
                tracing::warn!(error = %e, "Auto-sync after Prowlarr source creation failed");
            }
        });
    }

    // Auto-fetch caps for newznab/torznab sources
    if handler_type == "newznab" || handler_type == "torznab" {
        let _ = jobs::enqueue_optional(
            &state.pool,
            &state.nats.publisher,
            "refresh_indexer_caps",
            serde_json::json!({}),
            JobContext::new("Refresh indexer capabilities"),
        )
        .await;
    }

    let source = watch_repo::get_indexer_source_by_id(&state.pool, id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or_else(|| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Source not found after creation".to_string(),
            )
        })?;

    Ok(Json(source.into()))
}

/// Update an indexer source
pub(super) async fn update_indexer_source(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Json(request): Json<IndexerSourceRequest>,
) -> Result<Json<IndexerSourceResponse>, (StatusCode, String)> {
    if require_auth(&state, &headers).await.is_err() {
        return Err((StatusCode::UNAUTHORIZED, "Unauthorized".to_string()));
    }

    let handler_type = request.handler_type.clone();
    let input = watch_repo::IndexerSourceUpdate {
        name: Some(request.name),
        handler_type: Some(handler_type.clone()),
        handler_config: Some(request.handler_config),
        category_ids: Some(request.category_ids),
        media_type_ids: request.media_type_ids,
        enabled: Some(request.enabled),
        priority: Some(request.priority),
        seed_ratio: Some(request.seed_ratio),
        seed_time_minutes: Some(request.seed_time_minutes),
        search_mode: request.search_mode,
        auto_throttle: request.auto_throttle,
        api_limit_daily: request.api_limit_daily.map(Some),
        debrid_mode: request.debrid_mode,
    };

    let updated = watch_repo::update_indexer_source(&state.pool, id, input)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    if !updated {
        return Err((
            StatusCode::NOT_FOUND,
            "Indexer source not found".to_string(),
        ));
    }

    // Auto-trigger Prowlarr sync when updating a Prowlarr source
    if handler_type == "prowlarr" {
        let pool = state.pool.clone();
        let http = state.http_client.clone();
        tokio::spawn(async move {
            if let Err(e) = do_sync_prowlarr(&pool, &http, id).await {
                tracing::warn!(error = %e, "Auto-sync after Prowlarr source update failed");
            }
        });
    }

    // Auto-refresh caps for newznab/torznab sources
    if handler_type == "newznab" || handler_type == "torznab" {
        let _ = jobs::enqueue_optional(
            &state.pool,
            &state.nats.publisher,
            "refresh_indexer_caps",
            serde_json::json!({}),
            JobContext::new("Refresh indexer capabilities"),
        )
        .await;
    }

    let source = watch_repo::get_indexer_source_by_id(&state.pool, id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or_else(|| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Source not found after update".to_string(),
            )
        })?;

    Ok(Json(source.into()))
}

/// Delete an indexer source.
///
/// For Prowlarr sources, also deletes all child prowlarr_rss sources
/// (matched by base_url in handler_config).
pub(super) async fn delete_indexer_source(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED;
    }

    // Check if this is a Prowlarr source — cascade delete its RSS children
    if let Ok(Some(source)) = watch_repo::get_indexer_source_by_id(&state.pool, id).await
        && source.handler_type == "prowlarr"
    {
        let base_url = source
            .handler_config
            .get("base_url")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if let Ok(rss_sources) =
            watch_repo::list_indexer_sources_by_type(&state.pool, "prowlarr_rss").await
        {
            for rss in rss_sources {
                let rss_base = rss
                    .handler_config
                    .get("base_url")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if rss_base == base_url {
                    let _ = watch_repo::delete_indexer_source(&state.pool, rss.id).await;
                }
            }
        }
    }

    match watch_repo::delete_indexer_source(&state.pool, id).await {
        Ok(true) => StatusCode::NO_CONTENT,
        Ok(false) => StatusCode::NOT_FOUND,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

/// Trigger a manual check of an indexer source
pub(super) async fn trigger_indexer_check(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if require_auth(&state, &headers).await.is_err() {
        return Err((StatusCode::UNAUTHORIZED, "Unauthorized".to_string()));
    }

    // Reset last_check_at to trigger an immediate check
    let triggered = watch_repo::trigger_indexer_check(&state.pool, id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    if !triggered {
        return Err((
            StatusCode::NOT_FOUND,
            "Indexer source not found".to_string(),
        ));
    }

    Ok(Json(serde_json::json!({ "status": "check_triggered" })))
}

/// Reset indexer health (clear failures) and trigger immediate recheck
pub(super) async fn reset_indexer_health(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
) -> StatusCode {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED;
    }

    match watch_repo::reset_indexer_health(&state.pool, id).await {
        Ok(true) => StatusCode::NO_CONTENT,
        Ok(false) => StatusCode::NOT_FOUND,
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

/// Response for sync Prowlarr indexers
#[derive(Debug, Serialize)]
pub(super) struct SyncProwlarrResponse {
    created: i32,
    updated: i32,
    disabled: i32,
    indexers: Vec<String>,
}

/// Sync indexer sources from all configured Prowlarr instances.
/// Creates/updates prowlarr_rss sources for each enabled Prowlarr indexer.
pub(super) async fn sync_prowlarr_indexers(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> Result<Json<SyncProwlarrResponse>, (StatusCode, String)> {
    if require_auth(&state, &headers).await.is_err() {
        return Err((StatusCode::UNAUTHORIZED, "Unauthorized".to_string()));
    }

    let prowlarr_sources = watch_repo::list_indexer_sources_by_type(&state.pool, "prowlarr")
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    if prowlarr_sources.is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            "No Prowlarr indexer sources configured".to_string(),
        ));
    }

    let mut total = SyncProwlarrResponse {
        created: 0,
        updated: 0,
        disabled: 0,
        indexers: Vec::new(),
    };

    for source in &prowlarr_sources {
        if !source.enabled {
            continue;
        }
        match do_sync_prowlarr(&state.pool, &state.http_client, source.id).await {
            Ok(result) => {
                total.created += result.created;
                total.updated += result.updated;
                total.disabled += result.disabled;
                total.indexers.extend(result.indexers);
            }
            Err(e) => {
                tracing::warn!(
                    source_id = %source.id,
                    error = %e,
                    "Failed to sync Prowlarr instance"
                );
            }
        }
    }

    Ok(Json(total))
}

#[derive(Debug, Deserialize)]
pub(super) struct NewznabQuery {
    #[serde(flatten)]
    params: HashMap<String, String>,
}

/// Newznab-compatible API endpoint for Arr clients.
pub(super) async fn proxy_newznab(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<Uuid>,
    Query(query): Query<NewznabQuery>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    if require_auth(&state, &headers).await.is_err() {
        return Err((StatusCode::UNAUTHORIZED, "Unauthorized".to_string()));
    }

    let source = watch_repo::get_indexer_source_by_id(&state.pool, id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                "Indexer source not found".to_string(),
            )
        })?;

    if source.handler_type != "newznab" {
        return Err((
            StatusCode::BAD_REQUEST,
            "Indexer source is not newznab".to_string(),
        ));
    }

    let t = query
        .params
        .get("t")
        .map(|v| v.to_lowercase())
        .unwrap_or_else(|| "search".to_string());

    let base_url = if state.public_base_url.is_empty() {
        "http://localhost"
    } else {
        state.public_base_url.as_str()
    };

    if t == "caps" {
        let body = build_caps(base_url);
        let response = axum::response::Response::builder()
            .header(
                axum::http::header::CONTENT_TYPE,
                axum::http::HeaderValue::from_static("application/xml"),
            )
            .body(axum::body::Body::from(body))
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        return Ok(response);
    }

    let limit = normalize_limit(&query.params, 100);
    let offset = normalize_offset(&query.params);

    let categories = query
        .params
        .get("cat")
        .map(|value| {
            value
                .split(',')
                .filter_map(|cat| cat.trim().parse::<i32>().ok())
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    let handler = state.indexers.get("newznab").ok_or((
        StatusCode::INTERNAL_SERVER_ERROR,
        "Indexer handler not found".to_string(),
    ))?;

    let indexer_ctx = IndexerContext {
        pool: &state.pool,
        http_client: &state.http_client,
    };

    let query_value = normalize_query(&query.params);
    let search_request = IndexerSearchRequest {
        query: query_value.clone().unwrap_or_default(),
        categories,
        limit: Some(limit),
        offset: None,
        external_ids: Vec::new(),
        search_type: Default::default(),
        season: None,
        episode: None,
    };

    let releases = match t.as_str() {
        "rss" => handler
            .rss(&indexer_ctx, &source)
            .await
            .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?,
        "search" | "tvsearch" | "movie" => {
            let Some(query_value) = query_value else {
                return Err((StatusCode::BAD_REQUEST, "q required".to_string()));
            };
            handler
                .search(
                    &indexer_ctx,
                    Some(&source),
                    IndexerSearchRequest {
                        query: query_value,
                        categories: search_request.categories,
                        limit: search_request.limit,
                        offset: None,
                        external_ids: Vec::new(),
                        search_type: Default::default(),
                        season: None,
                        episode: None,
                    },
                )
                .await
                .map_err(|e| (StatusCode::BAD_GATEWAY, e.to_string()))?
        }
        _ => {
            return Err((
                StatusCode::BAD_REQUEST,
                format!("unsupported newznab query type '{}'", t),
            ));
        }
    };

    let body = build_search_response(base_url, &releases, offset);
    let mut response = axum::response::Response::new(body.into());
    response.headers_mut().insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("application/xml"),
    );
    Ok(response)
}

// =============================================================================
// Prowlarr → Native Migration
// =============================================================================

#[derive(Debug, Deserialize)]
pub(super) struct MigratePreviewRequest {
    prowlarr_source_id: Uuid,
}

/// Preview what native indexer sources would be created from a Prowlarr instance.
pub(super) async fn preview_prowlarr_migration(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<MigratePreviewRequest>,
) -> Result<Json<prowlarr_migrate::MigrationPreview>, (StatusCode, String)> {
    if require_auth(&state, &headers).await.is_err() {
        return Err((StatusCode::UNAUTHORIZED, "Unauthorized".to_string()));
    }

    let preview = prowlarr_migrate::preview_prowlarr_migration(
        &state.pool,
        &state.http_client,
        request.prowlarr_source_id,
    )
    .await
    .map_err(|e| (StatusCode::BAD_GATEWAY, e))?;

    Ok(Json(preview))
}

#[derive(Debug, Deserialize)]
pub(super) struct MigrateConfirmRequest {
    prowlarr_source_id: Uuid,
    items: Vec<prowlarr_migrate::MigrationConfirmItem>,
}

/// Create native indexer sources from confirmed Prowlarr migration items.
pub(super) async fn confirm_prowlarr_migration(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<MigrateConfirmRequest>,
) -> Result<Json<prowlarr_migrate::MigrationResult>, (StatusCode, String)> {
    if require_auth(&state, &headers).await.is_err() {
        return Err((StatusCode::UNAUTHORIZED, "Unauthorized".to_string()));
    }

    let result = prowlarr_migrate::confirm_prowlarr_migration(
        &state.pool,
        &state.http_client,
        request.prowlarr_source_id,
        request.items,
    )
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(result))
}
