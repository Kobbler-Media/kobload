use std::collections::HashSet;

use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::post,
};
use tracing::{info, warn};

use crate::auth::require_auth;
use crate::indexers::IndexerContext;
use crate::media::{
    match_targets_from_media_want, media_quality_overrides, media_quality_profile_id,
    search_terms_from_media_want,
};
use crate::repositories::media_types as media_types_repo;
use crate::repositories::quality_profiles as quality_repo;
use crate::scene_titles::TvNaming;
use crate::services::search::{SearchIndexersParams, search_indexers};
use crate::services::want::scoring::{
    ScoringConfig, SearchContext as ScoringSearchContext, score_batch,
};
use crate::state::AppState;
use crate::watch::{ProcessedRelease, Release, match_patterns, score_against_targets};
use nats_jobs::{SearchAndDownloadRequest, SearchAndDownloadResponse};

use super::helpers::create_download;

pub fn routes() -> Router<AppState> {
    Router::new().route("/api/v1/search-and-download", post(search_and_download))
}

/// Search for releases and return scored results.
/// Implements cascade search: patterns are tried in order, stopping early on good matches.
/// If auto_download is true and matches are found, downloads best match per target ID.
async fn search_and_download(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(mut request): Json<SearchAndDownloadRequest>,
) -> Result<Json<SearchAndDownloadResponse>, (StatusCode, String)> {
    if require_auth(&state, &headers).await.is_err() {
        return Err((StatusCode::UNAUTHORIZED, "Unauthorized".to_string()));
    }
    if let Some(want) = request.media_want.as_mut() {
        crate::learned_aliases::apply(&state.pool, want).await;
    }

    // If auto_download, require callback_url
    if request.auto_download && request.callback_url.as_ref().is_none_or(|u| u.is_empty()) {
        return Err((
            StatusCode::BAD_REQUEST,
            "callback_url required when auto_download is true".to_string(),
        ));
    }

    // Resolve media_type from key string
    let media_type_id = if let Some(ref mt) = request.media_type {
        media_types_repo::get_by_key(&state.pool, mt)
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

    let mut patterns = request.patterns.clone();
    let mut match_targets = request.match_targets.clone();
    let mut quality_profile = None;
    let mut quality_overrides = None;
    let mut naming = TvNaming::default();

    if let Some(media_want) = request.media_want.as_ref() {
        if patterns.is_empty() {
            patterns = search_terms_from_media_want(media_want);
        }
        if match_targets.is_empty() {
            match_targets = match_targets_from_media_want(media_want);
        }
        naming = TvNaming::resolve(
            &state.scene_mappings,
            &state.xem_cache,
            &state.http_client,
            &state.pool,
            media_want,
        )
        .await;
        naming.augment(media_want, &mut patterns, &mut match_targets);
        if let Some(profile_id) = media_quality_profile_id(media_want) {
            quality_profile = quality_repo::get_quality_profile_by_id(&state.pool, profile_id)
                .await
                .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        }
        quality_overrides = media_quality_overrides(media_want);
    }

    // Determine if we're using match_targets mode or legacy pattern matching
    let use_match_targets = !match_targets.is_empty();

    let early_exit_score = request.early_exit_score.unwrap_or(0.85);
    let limit = request.limit.unwrap_or(100);

    info!(
        patterns = ?patterns,
        match_targets_count = match_targets.len(),
        use_match_targets = use_match_targets,
        early_exit_score = early_exit_score,
        limit = limit,
        auto_download = request.auto_download,
        "Starting cascade search"
    );

    // Cascade search: try patterns in order, accumulate unique results
    let mut seen_guids: HashSet<String> = HashSet::new();
    let mut all_releases: Vec<Release> = Vec::new();
    let mut best_score: f64 = 0.0;
    let mut patterns_searched = 0;

    for pattern in &patterns {
        if pattern.trim().is_empty() {
            continue;
        }

        patterns_searched += 1;

        info!(
            pattern = %pattern,
            patterns_searched = patterns_searched,
            accumulated_results = all_releases.len(),
            "Searching cascade pattern"
        );

        // Search indexers with this pattern
        let releases = search_indexers(&SearchIndexersParams {
            ctx: &indexer_ctx,
            indexers: state.indexers.as_ref(),
            pool: &state.pool,
            query: pattern,
            limit,
            external_ids: &[],
            media_want: request.media_want.as_ref(),
            media_type_id,
            per_source_timeout: None,
            season: None,
            episode: None,
        })
        .await
        .map_err(|e| (StatusCode::BAD_GATEWAY, e))?;

        info!(
            pattern = %pattern,
            result_count = releases.len(),
            "Indexer search returned results"
        );

        // Accumulate unique results
        for release in releases {
            if seen_guids.insert(release.guid.clone()) {
                all_releases.push(release);
            }
        }

        // Score current results to check for early exit
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
            // Legacy mode: score against patterns
            for release in &all_releases {
                if let Some(match_result) =
                    match_patterns(&patterns, "fuzzy", request.min_score, &release.title)
                    && match_result.score > best_score
                {
                    best_score = match_result.score;
                }
            }
        }

        // Early exit if we found a good match
        if best_score >= early_exit_score {
            info!(
                pattern = %pattern,
                best_score = best_score,
                early_exit_score = early_exit_score,
                "Good match found, stopping cascade early"
            );
            break;
        }
    }

    info!(
        patterns_searched = patterns_searched,
        total_patterns = patterns.len(),
        total_results = all_releases.len(),
        best_score = best_score,
        "Cascade search complete"
    );

    // Score all results against match_targets or patterns
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
    .with_search_context(ScoringSearchContext::TextFallback);
    if let Some(ref mw) = request.media_want {
        config = config.with_media_want(mw);
    }
    config = naming.configure(config);
    if !use_match_targets {
        config = config.with_match_mode("fuzzy");
    }
    if let Some(overrides) = quality_overrides {
        config = config.with_quality_overrides(overrides);
    }
    let batch = score_batch(&all_releases, &config);
    let mut scored_results: Vec<&ProcessedRelease> =
        batch.iter().filter(|pr| pr.is_candidate()).collect();

    // Sort by total_score descending
    scored_results.sort_by(|a, b| {
        b.total_score
            .partial_cmp(&a.total_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    info!(
        scored_count = scored_results.len(),
        min_score = request.min_score,
        best_score = scored_results.first().map(|r| r.match_score.unwrap_or(0.0)),
        "Scored search results"
    );

    // Auto-download: best match per target ID (or single best if legacy mode)
    let mut download_id = None;
    let mut selected_release: Option<&ProcessedRelease> = None;

    if request.auto_download && !scored_results.is_empty() {
        let callback_url = request.callback_url.clone().unwrap();

        if use_match_targets {
            // Group by target_id and download best per target
            let mut by_target: std::collections::HashMap<
                Option<nats_jobs::MatchedTarget>,
                Vec<&ProcessedRelease>,
            > = std::collections::HashMap::new();
            for pr in &scored_results {
                by_target
                    .entry(pr.matched_target.clone())
                    .or_default()
                    .push(pr);
            }

            for (matched_target, results) in by_target {
                if let Some(best) = results.into_iter().max_by(|a, b| {
                    a.total_score
                        .partial_cmp(&b.total_score)
                        .unwrap_or(std::cmp::Ordering::Equal)
                }) && best.match_score.unwrap_or(0.0) >= request.min_score
                {
                    info!(
                        matched_target = ?matched_target,
                        title = %best.release.title,
                        score = best.match_score.unwrap_or(0.0),
                        "Auto-downloading best match for target"
                    );

                    let result = create_download(
                        &state,
                        best,
                        &callback_url,
                        &request.auth_token,
                        &request.metadata,
                        request
                            .media_want
                            .as_ref()
                            .map(crate::media::parse_media_kind)
                            .unwrap_or("misc"),
                    )
                    .await;

                    match result {
                        Ok(id) => {
                            if download_id.is_none() {
                                download_id = Some(id);
                                selected_release = Some(best);
                            }
                        }
                        Err(e) => {
                            warn!(
                                error = %e,
                                matched_target = ?matched_target,
                                title = %best.release.title,
                                "Failed to create download for target"
                            );
                        }
                    }
                }
            }
        } else {
            // Legacy mode: download single best match
            if let Some(best) = scored_results
                .first()
                .filter(|r| r.match_score.unwrap_or(0.0) >= request.min_score)
            {
                let result = create_download(
                    &state,
                    best,
                    &callback_url,
                    &request.auth_token,
                    &request.metadata,
                    request
                        .media_want
                        .as_ref()
                        .map(crate::media::parse_media_kind)
                        .unwrap_or("misc"),
                )
                .await;

                match result {
                    Ok(id) => {
                        download_id = Some(id);
                        selected_release = Some(best);
                    }
                    Err(e) => {
                        warn!(error = %e, "Failed to create download");
                        return Err((
                            StatusCode::INTERNAL_SERVER_ERROR,
                            format!("Failed to create download: {}", e),
                        ));
                    }
                }
            }
        }
    }

    Ok(Json(SearchAndDownloadResponse {
        download_id,
        selected_release: selected_release.map(|r| r.to_dto()),
        results: scored_results.into_iter().map(|r| r.to_dto()).collect(),
    }))
}
