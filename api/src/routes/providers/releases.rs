//! Provider release endpoints for batch interpretation and dry-run search.

use std::collections::HashSet;

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::post,
};
use kob_providers::releases::{
    InterpretInput, InterpretedQuality, InterpretedRelease, ReleaseSearchRequest,
    ReleaseSearchResults, ScoredRelease,
};
use serde_json::Value;
use tracing::info;
use uuid::Uuid;

use crate::auth::require_auth;
use crate::indexers::IndexerContext;
use crate::media::{
    match_targets_from_media_want, media_quality_overrides, media_quality_profile_id,
    search_terms_from_media_want,
};
use crate::parsing::parse_release;
use crate::quality::{parse_quality, score_quality};
use crate::repositories::media_types as media_types_repo;
use crate::repositories::quality_profiles as quality_repo;
use crate::services::search::{SearchIndexersParams, search_indexers};
use crate::services::want::{
    scoring::{ScoringConfig, SearchContext as ScoringSearchContext, score_batch},
    structural_reject, votes_reject,
};
use crate::state::AppState;
use crate::watch::{ProcessedRelease, Release, match_patterns, score_against_targets};
use nats_jobs::MediaWant;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/v1/providers/releases/interpret",
            post(interpret_batch),
        )
        .route("/api/v1/providers/releases/search", post(search_releases))
}

// =============================================================================
// Interpret endpoint
// =============================================================================

async fn interpret_batch(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(inputs): Json<Vec<InterpretInput>>,
) -> Result<Json<Vec<InterpretedRelease>>, StatusCode> {
    if require_auth(&state, &headers).await.is_err() {
        return Err(StatusCode::UNAUTHORIZED);
    }

    let results: Vec<InterpretedRelease> = inputs
        .into_iter()
        .map(|input| {
            let parsed = parse_release(&input.name, &input.media_kind);
            let media = serde_json::to_value(&parsed.media).unwrap_or(Value::Null);
            InterpretedRelease {
                input: input.name,
                title: parsed.title,
                quality: InterpretedQuality {
                    resolution: parsed.quality.resolution,
                    source: parsed.quality.source,
                    codec: parsed.quality.codec,
                    audio: parsed.quality.audio,
                    streaming_service: parsed.quality.streaming_service,
                    is_proper: parsed.quality.is_proper,
                    is_repack: parsed.quality.is_repack,
                    is_remux: parsed.quality.is_remux,
                    is_hdr: parsed.quality.is_hdr
                        || parsed.quality.is_hdr10
                        || parsed.quality.is_hdr10plus,
                    is_dolby_vision: parsed.quality.is_dolby_vision,
                    is_hardcoded_subs: parsed.quality.is_hardcoded_subs,
                },
                release_group: parsed.release_group,
                confidence: parsed.confidence,
                media,
            }
        })
        .collect();

    Ok(Json(results))
}

// =============================================================================
// Search endpoint
// =============================================================================

fn processed_to_release(pr: &ProcessedRelease) -> ScoredRelease {
    let pq = pr.parsed_quality.as_ref();

    ScoredRelease {
        guid: pr.release.guid.clone(),
        name: pr.release.title.clone(),
        total_score: pr.total_score,
        match_score: pr.match_score.unwrap_or(0.0),
        quality_score: pr.quality_score.unwrap_or(0.0),
        accepted: true,
        reject_reason: None,
        quality: pq
            .map(|q| InterpretedQuality {
                resolution: q.resolution.clone(),
                source: q.source.clone(),
                codec: q.codec.clone(),
                audio: q.audio.clone(),
                streaming_service: q.streaming_service.clone(),
                is_proper: q.is_proper,
                is_repack: q.is_repack,
                is_remux: q.is_remux,
                is_hdr: q.is_hdr || q.is_hdr10 || q.is_hdr10plus,
                is_dolby_vision: q.is_dolby_vision,
                is_hardcoded_subs: q.is_hardcoded_subs,
            })
            .unwrap_or_default(),
        release_group: pq.and_then(|q| q.release_group.clone()),
        indexer: Some(pr.release.indexer.clone()),
        size: Some(pr.release.size),
        matched_season: None,
        matched_episode: None,
        publish_date: Some(pr.release.publish_date.clone()),
        seeders: pr.release.seeders,
        votes: pr.release.votes,
        grabs: pr.release.grabs,
        protocol: Some(pr.release.protocol.clone()),
    }
}

async fn search_releases(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(request): Json<ReleaseSearchRequest>,
) -> Result<Json<ReleaseSearchResults>, (StatusCode, String)> {
    if require_auth(&state, &headers).await.is_err() {
        return Err((StatusCode::UNAUTHORIZED, "Unauthorized".to_string()));
    }

    // Deserialize the opaque media_want JSON into the typed MediaWant
    let media_want: MediaWant = serde_json::from_value(request.media_want).map_err(|e| {
        (
            StatusCode::UNPROCESSABLE_ENTITY,
            format!("invalid media_want: {e}"),
        )
    })?;
    let media_want = &media_want;

    // Resolve media_type_id for category filtering
    let media_type_key = media_type_key_from_want(media_want);
    let media_type_id = if let Some(ref key) = media_type_key {
        media_types_repo::get_by_key(&state.pool, key)
            .await
            .ok()
            .flatten()
            .map(|row| row.id)
    } else {
        None
    };

    let indexer_ctx = IndexerContext {
        pool: &state.pool,
        http_client: &state.http_client,
    };

    let patterns = search_terms_from_media_want(media_want);
    let match_targets = match_targets_from_media_want(media_want);

    // Resolve quality profile: explicit ID, media_want embedded, or default for kind
    let mut quality_profile = None;
    if let Some(ref profile_id_str) = request.quality_profile_id
        && let Ok(profile_id) = profile_id_str.parse::<Uuid>()
    {
        quality_profile = quality_repo::get_quality_profile_by_id(&state.pool, profile_id)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    }
    if quality_profile.is_none()
        && let Some(profile_id) = media_quality_profile_id(media_want)
    {
        quality_profile = quality_repo::get_quality_profile_by_id(&state.pool, profile_id)
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    }
    let quality_overrides = media_quality_overrides(media_want);

    let use_match_targets = !match_targets.is_empty();
    let early_exit_score = 0.85;
    let limit = request.limit.unwrap_or(100);

    info!(
        patterns = ?patterns,
        match_targets_count = match_targets.len(),
        "Provider search: starting cascade"
    );

    // Cascade search: try patterns in order, accumulate unique results
    let mut seen_guids: HashSet<String> = HashSet::new();
    let mut all_releases: Vec<Release> = Vec::new();
    let mut best_score: f64 = 0.0;

    for pattern in &patterns {
        if pattern.trim().is_empty() {
            continue;
        }

        let releases = search_indexers(&SearchIndexersParams {
            ctx: &indexer_ctx,
            indexers: state.indexers.as_ref(),
            pool: &state.pool,
            query: pattern,
            limit,
            external_ids: &[],
            media_want: Some(media_want),
            media_type_id,
            per_source_timeout: None,
            season: None,
            episode: None,
        })
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, e))?;

        for release in releases {
            if seen_guids.insert(release.guid.clone()) {
                all_releases.push(release);
            }
        }

        // Check for early exit
        if use_match_targets {
            for release in &all_releases {
                if let Some(match_result) =
                    score_against_targets(&release.title, &match_targets, request.min_score)
                    && match_result.score > best_score
                {
                    best_score = match_result.score;
                }
            }
        } else {
            for release in &all_releases {
                if let Some(match_result) =
                    match_patterns(&patterns, "fuzzy", request.min_score, &release.title)
                    && match_result.score > best_score
                {
                    best_score = match_result.score;
                }
            }
        }

        if best_score >= early_exit_score {
            info!(
                pattern = %pattern,
                best_score,
                "Provider search: good match found, stopping cascade"
            );
            break;
        }
    }

    let total_found = all_releases.len();

    // Score all results
    let trash_filters = state.trash_filters.get_filters();
    let mut config = ScoringConfig::new(
        if use_match_targets {
            &match_targets
        } else {
            &[]
        },
        if use_match_targets { &[] } else { &patterns },
        request.min_score,
        quality_profile.as_ref(),
        trash_filters.as_deref(),
    )
    .with_media_want(media_want)
    .with_search_context(ScoringSearchContext::TextFallback);
    if !use_match_targets {
        config = config.with_match_mode("fuzzy");
    }
    if let Some(overrides) = quality_overrides {
        config = config.with_quality_overrides(overrides);
    }
    let batch = score_batch(&all_releases, &config);
    let mut candidates_pr: Vec<&ProcessedRelease> =
        batch.iter().filter(|pr| pr.is_candidate()).collect();

    candidates_pr.sort_by(|a, b| {
        b.total_score
            .partial_cmp(&a.total_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let total_accepted = candidates_pr.len();

    let candidates: Vec<ScoredRelease> = candidates_pr
        .iter()
        .map(|pr| processed_to_release(pr))
        .collect();

    let best_pick = candidates.first().cloned();

    // Build rejected list: releases not in candidates, with reasons
    let accepted_guids: HashSet<&str> = candidates_pr
        .iter()
        .map(|pr| pr.release.guid.as_str())
        .collect();
    let rejected: Vec<ScoredRelease> = all_releases
        .iter()
        .filter(|r| !accepted_guids.contains(r.guid.as_str()))
        .map(|r| {
            let reason = structural_reject(r, Some(media_want), ScoringSearchContext::TextFallback)
                .map(|s| s.to_string())
                .or_else(|| votes_reject(r).map(|s| s.to_string()))
                .unwrap_or_else(|| {
                    let q = score_quality(
                        &r.title,
                        quality_profile.as_ref(),
                        quality_overrides,
                        trash_filters.as_deref(),
                    );
                    if q.rejected {
                        q.primary_reject_reason()
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| "quality_rejected".to_string())
                    } else {
                        "no_match".to_string()
                    }
                });

            let pq = parse_quality(&r.title);
            ScoredRelease {
                guid: r.guid.clone(),
                name: r.title.clone(),
                total_score: 0.0,
                match_score: 0.0,
                quality_score: 0.0,
                accepted: false,
                reject_reason: Some(reason),
                quality: InterpretedQuality {
                    resolution: pq.resolution.clone(),
                    source: pq.source.clone(),
                    codec: pq.codec.clone(),
                    audio: pq.audio.clone(),
                    streaming_service: pq.streaming_service.clone(),
                    is_proper: pq.is_proper,
                    is_repack: pq.is_repack,
                    is_remux: pq.is_remux,
                    is_hdr: pq.is_hdr || pq.is_hdr10 || pq.is_hdr10plus,
                    is_dolby_vision: pq.is_dolby_vision,
                    is_hardcoded_subs: pq.is_hardcoded_subs,
                },
                release_group: pq.release_group,
                indexer: Some(r.indexer.clone()),
                size: Some(r.size),
                matched_season: None,
                matched_episode: None,
                publish_date: Some(r.publish_date.clone()),
                seeders: r.seeders,
                votes: r.votes,
                grabs: r.grabs,
                protocol: Some(r.protocol.clone()),
            }
        })
        .collect();

    Ok(Json(ReleaseSearchResults {
        candidates,
        rejected,
        best_pick,
        total_found,
        total_accepted,
    }))
}

/// Extract a media_type key string from a MediaWant variant for category filtering.
fn media_type_key_from_want(want: &MediaWant) -> Option<String> {
    match want {
        MediaWant::TvEpisode { .. } | MediaWant::TvSeason { .. } => Some("tv".to_string()),
        MediaWant::Movie { .. } => Some("movies".to_string()),
        MediaWant::Book { .. } => Some("ebooks".to_string()),
        MediaWant::Audiobook { .. } => Some("audiobooks".to_string()),
        MediaWant::Music { .. } => Some("music".to_string()),
        MediaWant::Game { .. } => Some("games".to_string()),
        MediaWant::Software { .. } => Some("software".to_string()),
        MediaWant::Comic { .. } => Some("comics".to_string()),
        MediaWant::SportEvent { .. } | MediaWant::SportSeason { .. } => Some("sports".to_string()),
    }
}
