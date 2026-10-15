use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::{
        IntoResponse,
        sse::{Event, KeepAlive, Sse},
    },
};
use serde::{Deserialize, Serialize};
use std::convert::Infallible;
use std::time::Duration;
use tokio_stream::{StreamExt, wrappers::BroadcastStream};

use crate::auth::{require_auth, require_auth_with_token};
use crate::repositories::{
    downloads as downloads_repo, indexer_rss_cache, media_types as media_types_repo,
    watch as watch_repo,
};
use crate::state::AppState;
use crate::types::WatchPatternStreamEvent;
use crate::watch::{
    EpisodeProgress, ListWatchPatternsResponse, WatchMatchResponse, WatchPatternDetailResponse,
    WatchPatternResponse, WatchPatternUpdateRequest, WatchSyncItem, WatchSyncRequest,
    WatchSyncResponse, compute_pattern_status, episode_key,
};

fn default_pattern_limit() -> i64 {
    50
}

/// `From<WatchPatternRow>` can't see downloads, so on its own it reports every
/// pattern with an import as "searching". Responses that reach the UI (list,
/// stream snapshot, stream updates) go through one of these instead.
fn apply_download_state(
    resp: &mut WatchPatternResponse,
    ds: Option<&watch_repo::PatternDownloadState>,
) {
    resp.status = compute_pattern_status(
        resp.enabled,
        resp.upgrade_allowed,
        resp.upgrade_until,
        ds.is_some_and(|s| s.has_active),
        ds.is_some_and(|s| s.has_imported),
        ds.is_some_and(|s| s.has_held),
    );
}

async fn with_download_states(
    pool: &sqlx::PgPool,
    mut items: Vec<WatchPatternResponse>,
) -> Vec<WatchPatternResponse> {
    let download_states = watch_repo::batch_pattern_download_state(pool)
        .await
        .unwrap_or_default();
    let state_map: std::collections::HashMap<uuid::Uuid, &watch_repo::PatternDownloadState> =
        download_states.iter().map(|s| (s.pattern_id, s)).collect();
    for resp in &mut items {
        let ds = state_map.get(&resp.id).copied();
        apply_download_state(resp, ds);
    }
    items
}

async fn with_download_state(
    pool: &sqlx::PgPool,
    mut resp: WatchPatternResponse,
) -> WatchPatternResponse {
    let id = resp.id;
    let ds = watch_repo::PatternDownloadState {
        pattern_id: id,
        has_active: watch_repo::pattern_has_active_download(pool, id)
            .await
            .unwrap_or(false),
        has_imported: watch_repo::pattern_has_imported_download(pool, id)
            .await
            .unwrap_or(false),
        has_held: watch_repo::pattern_has_held_match(pool, id)
            .await
            .unwrap_or(false),
    };
    apply_download_state(&mut resp, Some(&ds));
    resp
}

#[derive(Debug, Deserialize)]
pub(super) struct WatchListQuery {
    #[serde(default)]
    source_system: Option<String>,
    cursor: Option<String>,
    #[serde(default = "default_pattern_limit")]
    limit: i64,
    enabled: Option<bool>,
    media_type_id: Option<String>,
    search: Option<String>,
}

/// Bulk sync watch patterns from a consumer
pub(super) async fn sync_watch_patterns(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<WatchSyncRequest>,
) -> Result<Json<WatchSyncResponse>, (StatusCode, String)> {
    if require_auth(&state, &headers).await.is_err() {
        return Err((StatusCode::UNAUTHORIZED, "Unauthorized".to_string()));
    }

    let source_system = &request.source_system;

    // Get existing patterns for this source system
    let existing = watch_repo::list_patterns_by_source(&state.pool, source_system)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let existing_ids: std::collections::HashSet<String> =
        existing.iter().map(|p| p.source_id.clone()).collect();
    let new_ids: std::collections::HashSet<String> =
        request.items.iter().map(|i| i.source_id.clone()).collect();

    let mut created = 0;
    let mut updated = 0;
    let mut deleted = 0;

    // Upsert each item
    for item in &request.items {
        let result = upsert_watch_pattern(&state, source_system, item).await;
        match result {
            Ok(true) => created += 1,
            Ok(false) => updated += 1,
            Err(e) => {
                return Err((StatusCode::INTERNAL_SERVER_ERROR, e.to_string()));
            }
        }
    }

    // Delete patterns that are no longer in the sync
    for source_id in existing_ids.difference(&new_ids) {
        let metadata = existing
            .iter()
            .find(|p| &p.source_id == source_id)
            .and_then(|p| p.metadata.clone());
        if watch_repo::delete_pattern_by_source(&state.pool, source_system, source_id)
            .await
            .is_ok()
        {
            crate::jobs::handlers::publish_watch_pattern_deleted(
                &state,
                source_system,
                source_id,
                metadata,
            )
            .await;
            deleted += 1;
        }
    }

    Ok(Json(WatchSyncResponse {
        created,
        updated,
        deleted,
    }))
}

async fn upsert_watch_pattern(
    state: &AppState,
    source_system: &str,
    item: &WatchSyncItem,
) -> Result<bool, crate::repositories::RepositoryError> {
    // Resolve media_type_id from media_want kind
    let media_type_id = resolve_media_type_from_want(&state.pool, item.media_want.as_ref()).await;

    let search_not_before = item.search_not_before.as_deref().and_then(|s| {
        time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).ok()
    });

    // Build sync input
    let input = watch_repo::WatchPatternSyncInput {
        source_system: source_system.to_string(),
        source_id: item.source_id.clone(),
        callback_url: Some(item.callback_url.clone()),
        patterns: item.patterns.clone(),
        media_want: item
            .media_want
            .as_ref()
            .and_then(|v| serde_json::to_value(v).ok()),
        match_targets: if item.match_targets.is_empty() {
            None
        } else {
            Some(serde_json::to_value(&item.match_targets).unwrap_or_default())
        },
        match_mode: item.match_mode.clone(),
        min_score: item.min_score,
        media_type_id,
        auto_download: item.auto_download,
        metadata: item.metadata.clone(),
        enabled: item.enabled,
        upgrade_allowed: item.upgrade_allowed,
        excluded_releases: None,
        search_not_before,
    };

    let exists = watch_repo::pattern_exists(&state.pool, source_system, &item.source_id).await?;

    if exists {
        watch_repo::update_pattern_by_source(&state.pool, &input).await?;
        Ok(false)
    } else {
        watch_repo::insert_pattern_from_sync(&state.pool, &input).await?;
        Ok(true)
    }
}

/// Resolve media_type_id from media_want kind by looking up the media_type_config table.
async fn resolve_media_type_from_want(
    pool: &sqlx::PgPool,
    media_want: Option<&nats_jobs::MediaWant>,
) -> Option<uuid::Uuid> {
    let key = match media_want? {
        nats_jobs::MediaWant::TvEpisode { .. }
        | nats_jobs::MediaWant::TvSeason { .. }
        | nats_jobs::MediaWant::SportEvent { .. }
        | nats_jobs::MediaWant::SportSeason { .. } => "tv",
        nats_jobs::MediaWant::Movie { .. } => "movie",
        nats_jobs::MediaWant::Book { .. } => "book",
        nats_jobs::MediaWant::Audiobook { .. } => "audiobook",
        nats_jobs::MediaWant::Comic { .. } => "comic",
        nats_jobs::MediaWant::Music { .. } => "music",
        nats_jobs::MediaWant::Game { .. } => "game",
        nats_jobs::MediaWant::Software { .. } => "software",
    };
    media_types_repo::get_by_key(pool, key)
        .await
        .ok()?
        .map(|mt| mt.id)
}

/// List all watch patterns with cursor pagination
pub(super) async fn list_watch_patterns(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<WatchListQuery>,
) -> Result<Json<ListWatchPatternsResponse>, (StatusCode, String)> {
    if require_auth(&state, &headers).await.is_err() {
        return Err((StatusCode::UNAUTHORIZED, "Unauthorized".to_string()));
    }

    let (cursor_ts, cursor_id) = query
        .cursor
        .as_ref()
        .and_then(|c| watch_repo::decode_pattern_cursor(c))
        .unzip();

    let media_type_id = query
        .media_type_id
        .as_ref()
        .and_then(|s| s.parse::<uuid::Uuid>().ok());

    let options = watch_repo::WatchPatternListOptions {
        limit: query.limit,
        cursor: cursor_ts,
        cursor_id,
        source_system: query.source_system,
        enabled: query.enabled,
        media_type_id,
        search: query.search,
    };

    let list_response = watch_repo::fetch_pattern_list(&state.pool, options)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let items = with_download_states(
        &state.pool,
        list_response.items.into_iter().map(Into::into).collect(),
    )
    .await;

    Ok(Json(ListWatchPatternsResponse {
        items,
        next_cursor: list_response.next_cursor,
    }))
}

#[derive(Debug, Deserialize)]
pub(super) struct PatternStreamQuery {
    pub token: Option<String>,
}

/// SSE stream for watch patterns - sends snapshot on connect, then live updates.
pub(super) async fn stream_watch_patterns(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<PatternStreamQuery>,
) -> impl IntoResponse {
    if require_auth_with_token(&state, &headers, query.token.as_deref())
        .await
        .is_err()
    {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    // Fetch initial snapshot (first page of patterns)
    let initial_patterns: Vec<WatchPatternResponse> = match watch_repo::fetch_pattern_list(
        &state.pool,
        watch_repo::WatchPatternListOptions {
            limit: 100,
            ..Default::default()
        },
    )
    .await
    {
        Ok(response) => {
            with_download_states(
                &state.pool,
                response.items.into_iter().map(Into::into).collect(),
            )
            .await
        }
        Err(err) => {
            tracing::warn!(error = %err, "Failed to fetch initial watch patterns for stream");
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
    };

    let initial_event = WatchPatternStreamEvent::Snapshot {
        patterns: initial_patterns,
    };

    let rx = state.watch_pattern_events.subscribe();

    let initial_stream = tokio_stream::once(Ok::<_, Infallible>(sse_event_from_pattern_event(
        initial_event,
    )));

    let broadcast_stream = BroadcastStream::new(rx).filter_map(|result| match result {
        Ok(event) => Some(Ok::<_, Infallible>(sse_event_from_pattern_event(event))),
        Err(_) => None,
    });

    let combined_stream = initial_stream.chain(broadcast_stream);

    Sse::new(combined_stream)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(30)))
        .into_response()
}

fn sse_event_from_pattern_event(event: WatchPatternStreamEvent) -> Event {
    let payload = serde_json::to_string(&event).unwrap_or_else(|_| "{}".to_string());
    Event::default().data(payload)
}

/// Create a watch pattern
pub(super) async fn create_watch_pattern(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<WatchPatternUpdateRequest>,
) -> Result<Json<WatchPatternResponse>, (StatusCode, String)> {
    if require_auth(&state, &headers).await.is_err() {
        return Err((StatusCode::UNAUTHORIZED, "Unauthorized".to_string()));
    }

    let input = watch_repo::WatchPatternInput {
        callback_url: request.callback_url,
        patterns: request.patterns,
        media_want: request
            .media_want
            .as_ref()
            .and_then(|v| serde_json::to_value(v).ok()),
        match_targets: request
            .match_targets
            .as_ref()
            .and_then(|v| serde_json::to_value(v).ok()),
        match_mode: Some(request.match_mode),
        min_score: Some(request.min_score),
        media_type_id: request.media_type_id,
        auto_download: request.auto_download,
        metadata: request.metadata,
        enabled: request.enabled,
        upgrade_allowed: request.upgrade_allowed,
        search_not_before: None,
    };

    let id = watch_repo::create_pattern(
        &state.pool,
        &request.source_system,
        &request.source_id,
        input,
    )
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let pattern = watch_repo::get_pattern_by_id(&state.pool, id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or_else(|| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Pattern not found after creation".to_string(),
            )
        })?;

    let response = with_download_state(&state.pool, pattern.into()).await;
    let _ = state
        .watch_pattern_events
        .send(WatchPatternStreamEvent::Created {
            watch_pattern: Box::new(response.clone()),
        });

    Ok(Json(response))
}

/// Update a watch pattern
pub(super) async fn update_watch_pattern(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<uuid::Uuid>,
    Json(request): Json<WatchPatternUpdateRequest>,
) -> Result<Json<WatchPatternResponse>, (StatusCode, String)> {
    if require_auth(&state, &headers).await.is_err() {
        return Err((StatusCode::UNAUTHORIZED, "Unauthorized".to_string()));
    }

    let input = watch_repo::WatchPatternInput {
        callback_url: request.callback_url,
        patterns: request.patterns,
        media_want: request
            .media_want
            .as_ref()
            .and_then(|v| serde_json::to_value(v).ok()),
        match_targets: request
            .match_targets
            .as_ref()
            .and_then(|v| serde_json::to_value(v).ok()),
        match_mode: Some(request.match_mode),
        min_score: Some(request.min_score),
        media_type_id: request.media_type_id,
        auto_download: request.auto_download,
        metadata: request.metadata,
        enabled: request.enabled,
        upgrade_allowed: request.upgrade_allowed,
        search_not_before: None,
    };

    let updated = watch_repo::update_pattern_by_id(&state.pool, id, input)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    if !updated {
        return Err((StatusCode::NOT_FOUND, "Watch pattern not found".to_string()));
    }

    // Fetch updated pattern
    let pattern = watch_repo::get_pattern_by_id(&state.pool, id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or_else(|| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Pattern not found after update".to_string(),
            )
        })?;

    let response = with_download_state(&state.pool, pattern.into()).await;
    let _ = state
        .watch_pattern_events
        .send(WatchPatternStreamEvent::Updated {
            watch_pattern: Box::new(response.clone()),
        });

    Ok(Json(response))
}

/// Update a watch pattern by source_system + source_id
pub(super) async fn update_watch_pattern_by_source(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((source_system, source_id)): Path<(String, String)>,
    Json(request): Json<WatchPatternUpdateRequest>,
) -> Result<Json<WatchPatternResponse>, (StatusCode, String)> {
    if require_auth(&state, &headers).await.is_err() {
        return Err((StatusCode::UNAUTHORIZED, "Unauthorized".to_string()));
    }

    let input = watch_repo::WatchPatternInput {
        callback_url: request.callback_url,
        patterns: request.patterns,
        media_want: request
            .media_want
            .as_ref()
            .and_then(|v| serde_json::to_value(v).ok()),
        match_targets: request
            .match_targets
            .as_ref()
            .and_then(|v| serde_json::to_value(v).ok()),
        match_mode: Some(request.match_mode),
        min_score: Some(request.min_score),
        media_type_id: request.media_type_id,
        auto_download: request.auto_download,
        metadata: request.metadata,
        enabled: request.enabled,
        upgrade_allowed: request.upgrade_allowed,
        search_not_before: None,
    };

    let updated = watch_repo::update_pattern_full(&state.pool, &source_system, &source_id, input)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    if !updated {
        return Err((StatusCode::NOT_FOUND, "Watch pattern not found".to_string()));
    }

    // Fetch updated pattern
    let pattern = watch_repo::get_pattern_by_source(&state.pool, &source_system, &source_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or_else(|| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Pattern not found after update".to_string(),
            )
        })?;

    let response = with_download_state(&state.pool, pattern.into()).await;
    let _ = state
        .watch_pattern_events
        .send(WatchPatternStreamEvent::Updated {
            watch_pattern: Box::new(response.clone()),
        });

    Ok(Json(response))
}

/// Request body for the test endpoint.
#[derive(Debug, Deserialize)]
pub(super) struct TestRequest {
    pub release_string: Option<String>,
}

/// Response from the test endpoint — returns the session_id for the decision log viewer.
#[derive(Debug, Serialize)]
pub(super) struct TestResponse {
    pub session_id: uuid::Uuid,
}

/// Dry-run test: match a watch pattern against cached RSS releases (or a single
/// release string) and record the full evaluation in the decision log.
///
/// Returns a `session_id` that the frontend can use to open the decision log detail view.
pub(super) async fn test_watch_pattern(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<uuid::Uuid>,
    body: Option<Json<TestRequest>>,
) -> Result<Json<TestResponse>, (StatusCode, String)> {
    use crate::decision_log::{DecisionCollector, LogProviderCall, LogRelease};
    use crate::quality::{parse_quality, score_quality};
    use crate::repositories::quality_profiles;

    if require_auth(&state, &headers).await.is_err() {
        return Err((StatusCode::UNAUTHORIZED, "Unauthorized".to_string()));
    }

    let pattern = watch_repo::get_pattern_by_id(&state.pool, id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, "Watch pattern not found".to_string()))?;

    let collector = DecisionCollector::new("test", Some(id), None, true);

    // Resolve quality profile for the pattern's media type
    let quality_profile = if let Some(mt_id) = pattern.media_type_id {
        let media_type = crate::repositories::media_types::get_by_id(&state.pool, mt_id)
            .await
            .ok()
            .flatten();
        if let Some(mt) = media_type {
            let kind = crate::media::media_type_key_to_quality_kind(&mt.key);
            quality_profiles::get_default_for_media_kind(&state.pool, kind)
                .await
                .ok()
                .flatten()
        } else {
            None
        }
    } else {
        None
    };

    let trash_filters = state.trash_filters.get_filters();
    let release_string = body.and_then(|Json(b)| b.release_string);

    if let Some(ref title) = release_string {
        // Single release string mode — score just this one string
        let call_id = collector.record_provider_call(LogProviderCall {
            id: uuid::Uuid::new_v4(),
            indexer_source_id: None,
            source_name: "manual_input".to_string(),
            handler_type: "test".to_string(),
            search_type: Some("manual".to_string()),
            query: Some(title.clone()),
            categories: None,
            external_ids: None,
            results_count: 1,
            results_after_dedup: 1,
            status: "ok".to_string(),
            error_message: None,
            duration_ms: Some(0),
        });

        let match_result =
            crate::watch::service::match_release_with_pattern(&pattern, title, None, &[]);
        let parsed = parse_quality(title);
        let quality_result = quality_profile
            .as_ref()
            .map(|p| score_quality(title, Some(p), None, trash_filters.as_deref()));

        let (decision, decision_reason, match_score, matched_pattern) =
            if let Some(ref mr) = match_result {
                if let Some(ref qr) = quality_result
                    && qr.rejected
                {
                    (
                        "rejected".to_string(),
                        qr.primary_reject_reason().map(|r| r.to_string()),
                        Some(mr.score),
                        Some(mr.matched_pattern.clone()),
                    )
                } else {
                    (
                        "candidate".to_string(),
                        None,
                        Some(mr.score),
                        Some(mr.matched_pattern.clone()),
                    )
                }
            } else {
                ("no_match".to_string(), None, None, None)
            };

        let quality_score = quality_result.as_ref().map(|q| q.score);
        let total = match_score.unwrap_or(0.0) + quality_score.unwrap_or(0.0);

        collector.record_release(LogRelease {
            provider_call_id: Some(call_id),
            release_title: title.clone(),
            release_guid: None,
            release_size: None,
            protocol: None,
            indexer_name: None,
            match_score,
            matched_pattern,
            match_mode: Some(pattern.match_mode.as_deref().unwrap_or("fuzzy").to_string()),
            quality_score,
            quality_score_breakdown: quality_result.as_ref().map(|q| q.breakdown.clone()),
            total_score: Some(total),
            parsed_quality: serde_json::to_value(&parsed).ok(),
            structural_result: None,
            decision,
            decision_reason,
            episode_key: None,
            is_best: match_result.is_some(),
            release_data: None,
        });

        if match_result.is_some() {
            collector.mark_best_by_title(title);
        }
    } else {
        // Cached RSS mode — score all cached releases against the pattern
        let sources = watch_repo::list_enabled_sources(&state.pool)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

        for source in &sources {
            let cache = indexer_rss_cache::get_rss_cache(&state.pool, source.id)
                .await
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

            let Some(cache) = cache else { continue };

            let releases = crate::prowlarr::parse_newznab_rss(&cache.response_xml, 0);
            let releases = match releases {
                Ok(r) => r,
                Err(_) => continue,
            };

            let call_id = collector.record_provider_call(LogProviderCall {
                id: uuid::Uuid::new_v4(),
                indexer_source_id: Some(source.id),
                source_name: source.name.clone(),
                handler_type: source.handler_type.clone(),
                search_type: Some("rss_cache".to_string()),
                query: None,
                categories: None,
                external_ids: None,
                results_count: releases.len() as i32,
                results_after_dedup: releases.len() as i32,
                status: "ok".to_string(),
                error_message: None,
                duration_ms: Some(0),
            });

            for release in &releases {
                let title = &release.title;
                let match_result =
                    crate::watch::service::match_release_with_pattern(&pattern, title, None, &[]);

                let parsed = parse_quality(title);
                let quality_result = quality_profile
                    .as_ref()
                    .map(|p| score_quality(title, Some(p), None, trash_filters.as_deref()));

                let (decision, decision_reason, match_score, matched_pattern) =
                    if let Some(ref mr) = match_result {
                        if let Some(ref qr) = quality_result
                            && qr.rejected
                        {
                            (
                                "rejected".to_string(),
                                qr.primary_reject_reason().map(|r| r.to_string()),
                                Some(mr.score),
                                Some(mr.matched_pattern.clone()),
                            )
                        } else {
                            (
                                "candidate".to_string(),
                                None,
                                Some(mr.score),
                                Some(mr.matched_pattern.clone()),
                            )
                        }
                    } else {
                        ("no_match".to_string(), None, None, None)
                    };

                let quality_score = quality_result.as_ref().map(|q| q.score);
                let total = match_score.unwrap_or(0.0) + quality_score.unwrap_or(0.0);

                collector.record_release(LogRelease {
                    provider_call_id: Some(call_id),
                    release_title: title.clone(),
                    release_guid: Some(release.guid.clone()),
                    release_size: Some(release.size),
                    protocol: release.protocol.clone(),
                    indexer_name: Some(source.name.clone()),
                    match_score,
                    matched_pattern,
                    match_mode: Some(pattern.match_mode.as_deref().unwrap_or("fuzzy").to_string()),
                    quality_score,
                    quality_score_breakdown: quality_result.as_ref().map(|q| q.breakdown.clone()),
                    total_score: Some(total),
                    parsed_quality: serde_json::to_value(&parsed).ok(),
                    structural_result: None,
                    decision,
                    decision_reason,
                    episode_key: None,
                    is_best: false,
                    release_data: None,
                });
            }
        }
    }

    collector.set_outcome("completed");
    let session_id = collector.session_id();

    // Flush synchronously since the frontend needs the session_id immediately
    crate::decision_log::repository::flush_session(&state.pool, collector.into_flush_payload())
        .await
        .map_err(|e| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Failed to flush decision log: {e}"),
            )
        })?;

    Ok(Json(TestResponse { session_id }))
}

/// Patch a watch pattern (partial update)
pub(super) async fn patch_watch_pattern(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path(id): Path<uuid::Uuid>,
    Json(request): Json<WatchPatternPatch>,
) -> Result<Json<WatchPatternResponse>, (StatusCode, String)> {
    if require_auth(&state, &headers).await.is_err() {
        return Err((StatusCode::UNAUTHORIZED, "Unauthorized".to_string()));
    }

    if let Some(enabled) = request.enabled {
        let updated = watch_repo::set_pattern_enabled(&state.pool, id, enabled)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        if !updated {
            return Err((StatusCode::NOT_FOUND, "Watch pattern not found".to_string()));
        }
    }

    let pattern = watch_repo::get_pattern_by_id(&state.pool, id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or_else(|| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                "Pattern not found after update".to_string(),
            )
        })?;

    let response = with_download_state(&state.pool, pattern.into()).await;
    let _ = state
        .watch_pattern_events
        .send(WatchPatternStreamEvent::Updated {
            watch_pattern: Box::new(response.clone()),
        });

    Ok(Json(response))
}

#[derive(Debug, Deserialize)]
pub(super) struct WatchPatternPatch {
    pub enabled: Option<bool>,
}

/// Get pattern detail with computed status and recent matches.
pub(super) async fn get_pattern_detail(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((source_system, source_id)): Path<(String, String)>,
) -> Result<Json<WatchPatternDetailResponse>, (StatusCode, String)> {
    if require_auth(&state, &headers).await.is_err() {
        return Err((StatusCode::UNAUTHORIZED, "Unauthorized".to_string()));
    }

    let pattern = watch_repo::get_pattern_by_source(&state.pool, &source_system, &source_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
        .ok_or_else(|| (StatusCode::NOT_FOUND, "Watch pattern not found".to_string()))?;

    let matches = watch_repo::list_matches_by_pattern(&state.pool, pattern.id, 50)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    let has_active = watch_repo::pattern_has_active_download(&state.pool, pattern.id)
        .await
        .unwrap_or(false);
    let has_imported = watch_repo::pattern_has_imported_download(&state.pool, pattern.id)
        .await
        .unwrap_or(false);
    let has_held = watch_repo::pattern_has_held_match(&state.pool, pattern.id)
        .await
        .unwrap_or(false);

    // Compute episode progress for TvSeason patterns
    let episode_progress = if let Some(ref mw) = pattern.media_want
        && let Some(episodes_needed) = episode_key::get_episodes_needed(mw)
    {
        let mut covered_keys = watch_repo::get_imported_episode_keys(&state.pool, pattern.id)
            .await
            .unwrap_or_default();
        // Episodes already in the library count as covered
        for key in episode_key::in_library_episode_keys(&episodes_needed) {
            if !covered_keys.contains(&key) {
                covered_keys.push(key);
            }
        }
        let imported_count = episodes_needed
            .iter()
            .filter(|ep| {
                covered_keys
                    .iter()
                    .any(|k| episode_key::key_covers_episode(k, &ep.episode))
            })
            .count();
        let downloading_count = episodes_needed.len().saturating_sub(imported_count);
        Some(EpisodeProgress {
            total: episodes_needed.len(),
            imported: imported_count,
            downloading: if has_active {
                downloading_count.min(1)
            } else {
                0
            },
        })
    } else {
        None
    };

    let is_season_partial = episode_progress
        .as_ref()
        .is_some_and(|ep| ep.imported < ep.total);

    let status = if is_season_partial && has_active {
        "downloading".to_string()
    } else if is_season_partial {
        "searching".to_string()
    } else {
        compute_pattern_status(
            pattern.enabled,
            pattern.upgrade_allowed,
            pattern.upgrade_until,
            has_active,
            has_imported,
            has_held,
        )
    };

    let match_targets = pattern.match_targets.as_ref().and_then(|v| {
        serde_json::from_value::<Vec<crate::watch::types::MatchTarget>>(v.clone()).ok()
    });
    let media_want = pattern
        .media_want
        .as_ref()
        .and_then(|v| serde_json::from_value::<nats_jobs::MediaWant>(v.clone()).ok());

    let pattern_response = WatchPatternResponse {
        id: pattern.id,
        source_system: pattern.source_system,
        source_id: pattern.source_id,
        callback_url: pattern.callback_url,
        patterns: pattern.patterns,
        media_want,
        match_targets,
        match_mode: pattern.match_mode.unwrap_or_else(|| "fuzzy".to_string()),
        min_score: pattern.min_score,
        media_type_id: pattern.media_type_id,
        auto_download: pattern.auto_download,
        metadata: pattern.metadata,
        enabled: pattern.enabled,
        upgrade_allowed: pattern.upgrade_allowed,
        upgrade_until: pattern.upgrade_until,
        status,
        episode_progress,
        created_at: pattern.created_at,
        updated_at: pattern.updated_at,
    };

    let match_responses: Vec<WatchMatchResponse> =
        matches.into_iter().map(WatchMatchResponse::from).collect();

    Ok(Json(WatchPatternDetailResponse {
        pattern: pattern_response,
        matches: match_responses,
    }))
}

/// Trigger an immediate search for a pattern.
pub(super) async fn search_pattern_now(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((source_system, source_id)): Path<(String, String)>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    if require_auth(&state, &headers).await.is_err() {
        return Err((StatusCode::UNAUTHORIZED, "Unauthorized".to_string()));
    }

    let found = crate::jobs::handlers::search_pattern_now_core(&state, &source_system, &source_id)
        .await
        .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(serde_json::json!({ "found": found })))
}

/// Grab the held match for a pattern.
pub(super) async fn grab_held_match(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((source_system, source_id)): Path<(String, String)>,
) -> Result<impl IntoResponse, (StatusCode, String)> {
    if require_auth(&state, &headers).await.is_err() {
        return Err((StatusCode::UNAUTHORIZED, "Unauthorized".to_string()));
    }

    let download_id =
        crate::jobs::handlers::grab_held_match_core(&state, &source_system, &source_id)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(serde_json::json!({ "download_id": download_id })))
}

#[derive(Debug, Deserialize)]
pub(super) struct BulkDeletePatternsRequest {
    pub ids: Vec<uuid::Uuid>,
}

#[derive(Debug, Serialize)]
pub(super) struct BulkDeletePatternsResponse {
    pub deleted: u64,
}

/// Bulk delete watch patterns.
pub(super) async fn bulk_delete_watch_patterns(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<BulkDeletePatternsRequest>,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return StatusCode::UNAUTHORIZED.into_response();
    }

    // Collect download IDs before cascade-deleting matches
    let download_ids = downloads_repo::get_download_ids_for_patterns(&state.pool, &body.ids)
        .await
        .unwrap_or_default();

    match watch_repo::bulk_delete_patterns(&state.pool, &body.ids).await {
        Ok(deleted) => {
            // Clean up orphaned hidden downloads
            let _ =
                downloads_repo::hard_delete_orphaned_hidden_downloads(&state.pool, &download_ids)
                    .await;

            // Broadcast delete events for each pattern
            for id in &body.ids {
                let _ = state
                    .watch_pattern_events
                    .send(crate::types::WatchPatternStreamEvent::Deleted { id: *id });
            }
            Json(BulkDeletePatternsResponse { deleted }).into_response()
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to bulk delete watch patterns: {}", e),
        )
            .into_response(),
    }
}

/// Delete a watch pattern
pub(super) async fn delete_watch_pattern(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((source_system, source_id)): Path<(String, String)>,
) -> impl IntoResponse {
    if require_auth(&state, &headers).await.is_err() {
        return (StatusCode::UNAUTHORIZED, "Unauthorized".to_string()).into_response();
    }

    // Fetch metadata before deleting so we can include it in the event
    let metadata = watch_repo::get_pattern_by_source(&state.pool, &source_system, &source_id)
        .await
        .ok()
        .flatten()
        .and_then(|p| p.metadata);

    // Get pattern ID before deletion for the stream event and orphan cleanup
    let pattern_id = watch_repo::get_pattern_by_source(&state.pool, &source_system, &source_id)
        .await
        .ok()
        .flatten()
        .map(|p| p.id);

    // Collect download IDs before cascade-deleting matches
    let download_ids = if let Some(pid) = pattern_id {
        downloads_repo::get_download_ids_for_pattern(&state.pool, pid)
            .await
            .unwrap_or_default()
    } else {
        vec![]
    };

    match watch_repo::delete_pattern_by_source(&state.pool, &source_system, &source_id).await {
        Ok(true) => {
            // Clean up orphaned hidden downloads (matches were cascade-deleted)
            let _ =
                downloads_repo::hard_delete_orphaned_hidden_downloads(&state.pool, &download_ids)
                    .await;

            crate::jobs::handlers::publish_watch_pattern_deleted(
                &state,
                &source_system,
                &source_id,
                metadata,
            )
            .await;
            if let Some(id) = pattern_id {
                let _ = state
                    .watch_pattern_events
                    .send(WatchPatternStreamEvent::Deleted { id });
            }
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => (StatusCode::NOT_FOUND, "not found").into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to delete watch pattern: {}", e),
        )
            .into_response(),
    }
}
