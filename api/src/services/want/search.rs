use std::collections::HashSet;

use serde_json::Value;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::decision_log::{DecisionCollector, LogRelease};
use crate::helpers::resolve_categories_for_media_type;
use crate::indexers::IndexerContext;
use crate::media::{
    episode_search_terms, external_ids_from_media_want, match_targets_from_media_want,
    media_quality_overrides, media_quality_profile_id, quality_media_kind,
    search_terms_from_media_want, season_episode_from_media_want,
};
use crate::repositories::watch as watch_repo;
use crate::repositories::{app_settings, quality_profiles as quality_repo};
use crate::scene_titles::{TvNaming, scene_numbering};
use crate::services::protocol::{ProtocolConfig, resolve_protocol_config};
use crate::services::search::{CascadedSearchParams, SearchContext, search_all_sources_cascaded};
#[cfg(test)]
use crate::services::search::{SearchIndexersParams, search_indexers};
use crate::watch::ExternalId;
use crate::watch::episode_key::extract_episode_key;
use crate::watch::{ProcessedRelease, Release};
use nats_jobs::MediaWant;

use super::downloads::create_download;
use super::scoring::{self, ScoringConfig, group_by_target_id, score_batch};
use super::types::{WantServiceContext, WantServiceRequest};

/// Check if the immediate search should hold rather than grab.
///
/// Returns true when a preferred protocol is configured with delay > 0,
/// there are eligible results above min_score, but none use the preferred protocol.
fn should_hold_immediate(
    candidates: &[ProcessedRelease],
    min_score: f64,
    pref: Option<&ProtocolConfig>,
) -> bool {
    let Some(pref) = pref else { return false };
    if pref.delay_minutes <= 0 {
        return false;
    }
    let Some(preferred) = pref.preferred() else {
        return false;
    };

    let has_preferred = candidates
        .iter()
        .any(|r| r.match_score.unwrap_or(0.0) >= min_score && r.release.protocol == preferred);
    let has_any = candidates
        .iter()
        .any(|r| r.match_score.unwrap_or(0.0) >= min_score);

    has_any && !has_preferred
}

/// Pick the best result, preferring the top-priority protocol.
///
/// Among results meeting `min_score`, if any results use the preferred protocol,
/// the best of those is returned. Falls back to the overall best if none match
/// the preferred protocol.
fn pick_best_with_protocol_preference<'a>(
    sorted_results: &'a [ProcessedRelease],
    min_score: f64,
    preferred_protocol: Option<&str>,
) -> Option<&'a ProcessedRelease> {
    let eligible: Vec<&ProcessedRelease> = sorted_results
        .iter()
        .filter(|r| r.match_score.unwrap_or(0.0) >= min_score)
        .collect();

    if let Some(preferred) = preferred_protocol
        && let Some(best_preferred) = eligible.iter().find(|r| r.release.protocol == preferred)
    {
        return Some(best_preferred);
    }

    // No preferred-protocol result, fall back to overall best
    eligible.into_iter().next()
}

pub(super) struct ImmediateSearchResult {
    pub(super) download_id: Option<Uuid>,
    pub(super) selected_release: Option<ProcessedRelease>,
    pub(super) results_found: usize,
    pub(super) best_score: f64,
    pub(super) matched_pattern: Option<String>,
    pub(super) matched_target: Option<nats_jobs::MatchedTarget>,
    pub(super) status: nats_jobs::WantStatus,
    /// Whether a non-preferred protocol result was held instead of downloaded.
    /// Used internally by do_immediate_search to gate episode fallback; not read by callers.
    #[allow(dead_code)]
    pub(super) protocol_held: bool,
    /// For TvSeason: episode identifiers that actually got downloads.
    pub(super) episodes_downloaded: Vec<String>,
}

/// Check if a release title matches any excluded release entry.
fn is_excluded_by_json(excluded: &Value, release_title: &str) -> bool {
    let arr = match excluded.as_array() {
        Some(a) => a,
        None => return false,
    };
    for entry in arr {
        if let Some(title) = entry.get("release_title").and_then(|v| v.as_str())
            && !title.is_empty()
            && title == release_title
        {
            return true;
        }
        if let Some(hash) = entry.get("info_hash").and_then(|v| v.as_str())
            && !hash.is_empty()
            && release_title.contains(hash)
        {
            return true;
        }
    }
    false
}

pub(super) async fn do_immediate_search(
    ctx: &WantServiceContext<'_>,
    request: &WantServiceRequest,
    collector: Option<&DecisionCollector>,
) -> Result<ImmediateSearchResult, String> {
    let indexer_ctx = IndexerContext {
        pool: ctx.pool,
        http_client: ctx.http_client,
    };

    let mut patterns = request.patterns.clone();
    let mut match_targets = request.match_targets.clone();
    let mut quality_profile = None;
    let mut quality_overrides = None;
    let mut external_ids: Vec<ExternalId> = Vec::new();

    let mut naming = TvNaming::default();

    if let Some(media_want) = request.media_want.as_ref() {
        if patterns.is_empty() {
            patterns = search_terms_from_media_want(media_want);
        }
        if match_targets.is_empty() {
            match_targets = match_targets_from_media_want(media_want);
        }

        naming = TvNaming::resolve(
            ctx.scene_mappings,
            ctx.xem_cache,
            ctx.http_client,
            ctx.pool,
            media_want,
        )
        .await;
        if let MediaWant::TvEpisode { show_name, .. } = media_want
            && let Some(show_map) = &naming.xem_show_map
            && let Some(scene) = scene_numbering(media_want, show_map)
            && let Some(scene_episode) = scene.episode
        {
            debug!(
                tvdb_season = scene.tvdb_season,
                tvdb_episode = ?scene.tvdb_episode,
                scene_season = scene.season,
                scene_episode,
                "XEM scene numbering differs, adding extra search terms"
            );
            for term in episode_search_terms(
                show_name,
                &scene.season.to_string(),
                &scene_episode.to_string(),
            ) {
                if !patterns.contains(&term) {
                    patterns.push(term);
                }
            }
        }
        naming.augment(media_want, &mut patterns, &mut match_targets);

        if let Some(profile_id) = media_quality_profile_id(media_want) {
            quality_profile = quality_repo::get_quality_profile_by_id(ctx.pool, profile_id)
                .await
                .map_err(|e| e.to_string())?;
        }
        quality_overrides = media_quality_overrides(media_want);
        // Fall back to default for the media kind if no explicit profile
        if quality_profile.is_none()
            && let Some(kind) = quality_media_kind(media_want)
        {
            quality_profile = quality_repo::get_default_for_media_kind(ctx.pool, kind)
                .await
                .map_err(|e| e.to_string())?;
        }
        external_ids = external_ids_from_media_want(media_want);
    }

    let use_match_targets = !match_targets.is_empty();

    // Extract season/episode for structured Newznab search params
    let (season, episode) = request
        .media_want
        .as_ref()
        .map(season_episode_from_media_want)
        .unwrap_or((None, None));

    info!(
        patterns = ?patterns,
        match_targets_count = match_targets.len(),
        use_match_targets = use_match_targets,
        has_external_ids = !external_ids.is_empty(),
        season = ?season,
        episode = ?episode,
        "Starting immediate search (structured→fallback cascade)"
    );

    // Record search patterns in decision log for debugging
    if let Some(log) = collector {
        for p in &patterns {
            log.record_query(p);
        }
    }

    // Sonarr-style: per-source structured→fallback cascade.
    // For each source: try ID-based structured search first (no free-text query),
    // then fall back to the text pattern cascade if structured returned nothing.
    // Want searches are interactive (triggered by user action or immediate want processing)
    let raw_releases = search_all_sources_cascaded(&CascadedSearchParams {
        ctx: &indexer_ctx,
        indexers: ctx.indexers,
        pool: ctx.pool,
        patterns: &patterns,
        limit: request.limit,
        external_ids: &external_ids,
        media_want: request.media_want.as_ref(),
        media_type_id: request.media_type_id,
        season: season.as_deref(),
        episode: episode.as_deref(),
        search_context: SearchContext::Interactive,
        early_exit_check: None,
    })
    .await?;

    let mut seen_guids: HashSet<String> = HashSet::new();
    let mut all_releases: Vec<Release> = Vec::new();

    for release in raw_releases {
        if seen_guids.insert(release.guid.clone()) {
            // Skip excluded releases (previously rejected/replaced by user)
            if let Some(ref excluded) = request.excluded_releases
                && is_excluded_by_json(excluded, &release.title)
            {
                continue;
            }
            all_releases.push(release);
        }
    }

    // Post-filter releases by category to catch results that slipped through pre-filters
    let allowed_categories = if let Some(mt_id) = request.media_type_id {
        let cats = resolve_categories_for_media_type(ctx.pool, Some(mt_id))
            .await
            .unwrap_or_default();
        if !cats.is_empty() { Some(cats) } else { None }
    } else {
        None
    };

    // Build unified scoring config
    let mut scoring_config = ScoringConfig::new(
        &match_targets,
        &patterns,
        request.min_score,
        quality_profile.as_ref(),
        ctx.trash_filters.as_deref(),
    )
    .with_search_context(scoring::SearchContext::TextFallback);

    if !use_match_targets {
        scoring_config = scoring_config.with_match_mode(&request.match_mode);
    }
    if let Some(overrides) = quality_overrides {
        scoring_config = scoring_config.with_quality_overrides(overrides);
    }
    if let Some(mw) = request.media_want.as_ref() {
        scoring_config = scoring_config.with_media_want(mw);
    }
    scoring_config = naming.configure(scoring_config);
    if let Some(ref cats) = allowed_categories {
        scoring_config = scoring_config.with_allowed_categories(cats);
    }

    let processed_releases = score_batch(&all_releases, &scoring_config);

    // Best score across all releases (candidates and rejected alike)
    let best_score = processed_releases
        .iter()
        .filter_map(|pr| pr.match_score)
        .fold(0.0_f64, f64::max);

    // Collect candidates and sort by total_score descending
    let mut scored_results: Vec<ProcessedRelease> = processed_releases
        .iter()
        .filter(|pr| pr.is_candidate())
        .cloned()
        .collect();

    scored_results.sort_by(|a, b| {
        b.total_score
            .partial_cmp(&a.total_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    // Koblem LLM review — fire-and-forget via NATS using pre-processed releases.
    let koblem_settings = app_settings::get_koblem_settings(ctx.pool)
        .await
        .unwrap_or_default();
    if koblem_settings.enabled && ctx.nats.is_available() {
        super::koblem_review::publish_review(
            ctx.nats,
            &processed_releases,
            request.media_want.as_ref(),
            quality_profile.as_ref(),
            &request.source_id,
            "backlog",
        )
        .await;
    }

    // For TvSeason, filter out episodes the user already has
    if let Some(MediaWant::TvSeason {
        episodes_needed: Some(ref needed),
        ..
    }) = request.media_want
        && !needed.is_empty()
    {
        let before = scored_results.len();
        scored_results.retain(|r| {
            let mw_json = request
                .media_want
                .as_ref()
                .and_then(|mw| serde_json::to_value(mw).ok());
            let Some(mw_json) = mw_json else {
                return true;
            };
            let Some(key) = extract_episode_key(&r.release.title, &mw_json) else {
                return true; // can't determine episode — keep it
            };
            crate::watch::episode_key::is_episode_needed(&key, needed)
        });
        let filtered = before - scored_results.len();
        if filtered > 0 {
            debug!(
                filtered,
                remaining = scored_results.len(),
                "Filtered scored results by episodes_needed"
            );
        }
    }

    // Record ALL evaluated releases in decision log — ProcessedRelease has
    // the full pipeline result for every release (candidates and rejects).
    if let Some(log) = collector {
        let match_mode_str = if use_match_targets {
            "match_targets"
        } else {
            "pattern"
        };

        for pr in &processed_releases {
            let decision_reason = pr
                .reject_reasons
                .first()
                .map(|r| r.to_string())
                .or_else(|| pr.quality_reject_reasons.first().map(|r| r.to_string()));
            let structural_result =
                pr.structural_reject_reason
                    .map(|reason| crate::decision_log::StructuralResult {
                        checked: true,
                        matched: false,
                        reject_reason: Some(format!("{reason:?}")),
                        parsed_season: None,
                        parsed_episode: None,
                        parsed_year: None,
                        parsed_title: None,
                        want_season: None,
                        want_episode: None,
                        want_year: None,
                    });
            log.record_release(LogRelease {
                provider_call_id: None,
                release_title: pr.release.title.clone(),
                release_guid: Some(pr.release.guid.clone()),
                release_size: Some(pr.release.size),
                protocol: Some(pr.release.protocol.clone()),
                indexer_name: Some(pr.release.indexer.clone()),
                match_score: pr.match_score,
                matched_pattern: pr.matched_pattern.clone(),
                match_mode: Some(match_mode_str.to_string()),
                quality_score: pr.quality_score,
                quality_score_breakdown: None,
                total_score: Some(pr.total_score),
                parsed_quality: pr
                    .parsed_quality
                    .as_ref()
                    .and_then(|pq| serde_json::to_value(pq).ok()),
                structural_result,
                match_explanation: pr.match_explanation.clone(),
                decision: pr.decision.to_string(),
                decision_reason,
                episode_key: pr.episode_key.clone(),
                is_best: false,
                release_data: None,
            });
        }
    }

    // Resolve protocol preference (e.g., usenet > torrent) for smarter selection
    let protocol_pref = resolve_protocol_config(ctx.pool, request.media_type_id).await;
    let preferred_protocol_str = protocol_pref.as_ref().and_then(|p| p.preferred());

    // Resolve episode upgrade context for TvSeason (library/upgrade checks).
    let episode_upgrade_ctx = {
        let episodes_needed = request
            .media_want
            .as_ref()
            .and_then(|mw| serde_json::to_value(mw).ok())
            .as_ref()
            .and_then(crate::watch::episode_key::get_episodes_needed);
        super::EpisodeUpgradeContext {
            episodes_needed,
            upgrade_allowed: request.upgrade_allowed,
            manual: request.manual,
            pattern_id: request.watch_pattern_id,
        }
    };

    let mut download_id = None;
    let mut selected_release = None;
    let mut matched_pattern = None;
    let mut matched_target = None;
    let mut protocol_held = false;
    let mut episodes_downloaded: Vec<String> = Vec::new();

    if request.auto_download && !scored_results.is_empty() {
        // Check if we should hold non-preferred results instead of grabbing immediately
        if should_hold_immediate(&scored_results, request.min_score, protocol_pref.as_ref())
            && let Some(watch_pattern_id) = request.watch_pattern_id
        {
            // Pick the best overall result (ignoring protocol preference)
            if let Some(best) =
                pick_best_with_protocol_preference(&scored_results, request.min_score, None)
            {
                let delay = protocol_pref.as_ref().unwrap().delay_minutes;
                let hold_until =
                    time::OffsetDateTime::now_utc() + time::Duration::minutes(delay as i64);

                let release_data = serde_json::json!({
                    "download_url": best.release.download_url,
                    "magnet_url": best.release.magnet_url,
                    "protocol": best.release.protocol,
                    "indexer": best.release.indexer,
                    "size": best.release.size,
                    "indexer_source_id": best.release.indexer_source_id,
                    "guid": best.release.guid,
                    "title": best.release.title,
                    "score": best.match_score.unwrap_or(0.0),
                    "quality_score": best.quality_score,
                    "total_score": best.total_score,
                    "matched_pattern": best.matched_pattern,
                    "matched_target": best.matched_target,
                });

                let episode_key = request.media_want.as_ref().and_then(|mw| {
                    let mw_json = serde_json::to_value(mw).ok()?;
                    extract_episode_key(&best.release.title, &mw_json)
                });

                let match_input = watch_repo::WatchMatchInput {
                    pattern_id: watch_pattern_id,
                    indexer_source_id: best.release.indexer_source_id,
                    release_guid: &best.release.guid,
                    release_title: &best.release.title,
                    release_data: Some(&release_data),
                    match_score: best.total_score,
                    matched_pattern: best.matched_pattern.as_deref().unwrap_or(""),
                    action: "held",
                    download_id: None,
                    hold_until: Some(hold_until),
                    held: true,
                    quality_score: best.quality_score,
                    rejected: false,
                    rejected_at: None,
                    rejected_reason: None,
                    episode_key: episode_key.as_deref(),
                };

                match watch_repo::create_match_if_not_exists(ctx.pool, &match_input).await {
                    Ok(Some(_match_id)) => {
                        info!(
                            pattern_id = %watch_pattern_id,
                            release = %best.release.title,
                            protocol = %best.release.protocol,
                            hold_until = %hold_until,
                            delay_minutes = delay,
                            "Holding non-preferred result for protocol delay \
                             (preferred: {:?})",
                            protocol_pref.as_ref().and_then(|p| p.preferred()),
                        );
                        selected_release = Some(best.clone());
                        matched_pattern = best.matched_pattern.clone();
                        matched_target = best.matched_target.clone();
                        protocol_held = true;
                    }
                    Ok(None) => {
                        debug!(
                            pattern_id = %watch_pattern_id,
                            release = %best.release.title,
                            "Race avoided: hold already exists for this release"
                        );
                        protocol_held = true;
                    }
                    Err(e) => {
                        warn!(
                            error = %e,
                            "Failed to create protocol hold, falling back to immediate download"
                        );
                        // Fall through to normal download path below
                    }
                }
            }
        }

        // Only proceed with immediate download if we didn't hold
        if !protocol_held {
            if use_match_targets {
                let by_target = group_by_target_id(&scored_results);

                for (_target_id, results) in by_target {
                    let mut owned: Vec<ProcessedRelease> = results.into_iter().cloned().collect();
                    owned.sort_by(|a, b| {
                        b.total_score
                            .partial_cmp(&a.total_score)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    });
                    if let Some(best) = pick_best_with_protocol_preference(
                        &owned,
                        request.min_score,
                        preferred_protocol_str,
                    ) {
                        if best.release.protocol
                            != owned
                                .first()
                                .map(|r| r.release.protocol.as_str())
                                .unwrap_or("")
                        {
                            info!(
                                preferred = preferred_protocol_str,
                                selected_protocol = %best.release.protocol,
                                "Protocol preference applied: chose {} over higher-scored result",
                                best.release.protocol,
                            );
                        }

                        // Create match first to win the race — only download if we won
                        let match_id = reserve_watch_match_for_search(
                            ctx.pool,
                            request.watch_pattern_id,
                            best,
                            request.media_want.as_ref(),
                            request.manual,
                        )
                        .await;

                        let Some(match_id) = match_id else {
                            debug!(
                                release = %best.release.title,
                                "Race avoided: match already exists, skipping download"
                            );
                            continue;
                        };

                        // Episode upgrade/library check (TvSeason only)
                        if matches!(
                            request.media_want,
                            Some(nats_jobs::MediaWant::TvSeason { .. })
                        ) {
                            let ep_key = request.media_want.as_ref().and_then(|mw| {
                                let mw_json = serde_json::to_value(mw).ok()?;
                                extract_episode_key(&best.release.title, &mw_json)
                            });
                            if let Some(ref key) = ep_key
                                && let Some(reason) = super::should_skip_episode(
                                    ctx.pool,
                                    key,
                                    &best.release.title,
                                    Some(&best.release.publish_date),
                                    &episode_upgrade_ctx,
                                    quality_profile.as_ref(),
                                    ctx.trash_filters.as_ref(),
                                )
                                .await
                            {
                                if let Some(mid) = match_id {
                                    let _ =
                                        watch_repo::update_match_skipped(ctx.pool, mid, &reason)
                                            .await;
                                }
                                if let Some(log) = collector {
                                    log.record_release(LogRelease {
                                        provider_call_id: None,
                                        release_title: best.release.title.clone(),
                                        release_guid: Some(best.release.guid.clone()),
                                        release_size: Some(best.release.size),
                                        protocol: Some(best.release.protocol.clone()),
                                        indexer_name: Some(best.release.indexer.clone()),
                                        match_score: best.match_score,
                                        matched_pattern: best.matched_pattern.clone(),
                                        match_mode: None,
                                        quality_score: best.quality_score,
                                        quality_score_breakdown: None,
                                        total_score: Some(best.total_score),
                                        parsed_quality: best
                                            .parsed_quality
                                            .as_ref()
                                            .and_then(|pq| serde_json::to_value(pq).ok()),
                                        structural_result: None,
                                        match_explanation: best.match_explanation.clone(),
                                        decision: "skipped".to_string(),
                                        decision_reason: Some(reason),
                                        episode_key: ep_key,
                                        is_best: false,
                                        release_data: None,
                                    });
                                }
                                continue;
                            }
                        }

                        let id = create_download(
                            ctx,
                            best,
                            &request.callback_url,
                            &request.auth_token,
                            &request.metadata,
                            request
                                .media_want
                                .as_ref()
                                .map(crate::media::parse_media_kind)
                                .unwrap_or("misc"),
                        )
                        .await?;

                        // Update the match with the download ID
                        if let Some(mid) = match_id {
                            let _ = watch_repo::update_match_download_id(ctx.pool, mid, id).await;
                        }

                        if download_id.is_none() {
                            download_id = Some(id);
                            selected_release = Some(best.clone());
                            matched_pattern = best.matched_pattern.clone();
                            matched_target = best.matched_target.clone();
                        }

                        // For non-TvSeason: one download is enough, don't grab multiple targets
                        if !matches!(
                            request.media_want,
                            Some(nats_jobs::MediaWant::TvSeason { .. })
                        ) {
                            break;
                        }
                    }
                }
            } else if let Some(best) = pick_best_with_protocol_preference(
                &scored_results,
                request.min_score,
                preferred_protocol_str,
            ) {
                if best.release.protocol
                    != scored_results
                        .first()
                        .map(|r| r.release.protocol.as_str())
                        .unwrap_or("")
                {
                    info!(
                        preferred = preferred_protocol_str,
                        selected_protocol = %best.release.protocol,
                        "Protocol preference applied: chose {} over higher-scored result",
                        best.release.protocol,
                    );
                }

                // Create match first to win the race — only download if we won
                let match_id = reserve_watch_match_for_search(
                    ctx.pool,
                    request.watch_pattern_id,
                    best,
                    request.media_want.as_ref(),
                    request.manual,
                )
                .await;

                if let Some(match_id) = match_id {
                    let id = match create_download(
                        ctx,
                        best,
                        &request.callback_url,
                        &request.auth_token,
                        &request.metadata,
                        request
                            .media_want
                            .as_ref()
                            .map(crate::media::parse_media_kind)
                            .unwrap_or("misc"),
                    )
                    .await
                    {
                        Ok(id) => id,
                        Err(e) => {
                            if let Some(mid) = match_id {
                                let reason = format!("create_download failed: {e}");
                                let _ =
                                    watch_repo::update_match_skipped(ctx.pool, mid, &reason).await;
                            }
                            return Err(e);
                        }
                    };

                    if let Some(mid) = match_id {
                        let _ = watch_repo::update_match_download_id(ctx.pool, mid, id).await;
                    }

                    download_id = Some(id);
                    selected_release = Some(best.clone());
                    matched_pattern = best.matched_pattern.clone();
                    matched_target = best.matched_target.clone();
                } else {
                    debug!(
                        release = %best.release.title,
                        "Race avoided: match already exists, skipping download"
                    );
                }
            }
        }
    }

    // Episode fallback for TvSeason: try individual episodes for any gaps.
    // Always runs when there are episodes_needed, even if some were already downloaded
    // by the season-level search — the fallback handles duplicates via reserve_episode_match.
    // Skip if we created a protocol hold — the hold expiry will handle it.
    if !protocol_held
        && let Some(MediaWant::TvSeason {
            ref show_name,
            ref season,
            episodes_needed: Some(ref needed),
            ..
        }) = request.media_want
        && !needed.is_empty()
    {
        info!(
            episodes_count = needed.len(),
            "Trying episode fallback for remaining gaps"
        );

        let fallback = super::episode_fallback::run_episode_fallback(
            ctx,
            show_name,
            season,
            needed,
            &all_releases,
            request.media_want.as_ref().unwrap(),
            request.min_score,
            &request.callback_url,
            &request.auth_token,
            &request.metadata,
            quality_profile.as_ref(),
            quality_overrides,
            ctx.indexers,
            &external_ids,
            request.media_type_id,
            request.limit,
            request.watch_pattern_id,
            preferred_protocol_str,
            &episode_upgrade_ctx,
        )
        .await?;

        if let Some(id) = fallback.first_download_id {
            download_id = Some(id);
            selected_release = fallback.first_release;
        }

        if !fallback.episodes_missing.is_empty() {
            info!(
                downloaded = fallback.episodes_downloaded.len(),
                missing = fallback.episodes_missing.len(),
                "Episode fallback: partial coverage, remaining will be watched via RSS"
            );
        } else if !fallback.episodes_downloaded.is_empty() {
            info!(
                downloaded = fallback.episodes_downloaded.len(),
                "Episode fallback: all episodes found"
            );
        }

        episodes_downloaded = fallback.episodes_downloaded;
    }

    let status = if download_id.is_some() {
        nats_jobs::WantStatus::Downloading
    } else {
        nats_jobs::WantStatus::Watching
    };

    Ok(ImmediateSearchResult {
        download_id,
        selected_release,
        results_found: scored_results.len(),
        best_score: best_score.max(
            scored_results
                .first()
                .map(|r| r.match_score.unwrap_or(0.0))
                .unwrap_or(0.0),
        ),
        matched_pattern,
        matched_target,
        status,
        protocol_held,
        episodes_downloaded,
    })
}

/// Compute episode key and delegate to the shared reserve_watch_match.
async fn reserve_watch_match_for_search(
    pool: &sqlx::PgPool,
    watch_pattern_id: Option<Uuid>,
    best: &ProcessedRelease,
    media_want: Option<&MediaWant>,
    manual: bool,
) -> Option<Option<Uuid>> {
    let episode_key = media_want.and_then(|mw| {
        let mw_json = serde_json::to_value(mw).ok()?;
        extract_episode_key(&best.release.title, &mw_json)
    });
    super::reserve_watch_match(pool, watch_pattern_id, best, episode_key.as_deref(), manual).await
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::indexers::DefaultIndexerResolver;
    use crate::repositories::watch as watch_repo;
    use crate::test_utils::{test_db_lock, test_pool};

    #[tokio::test]
    async fn test_search_indexers_queries_native_sources() {
        let _lock = test_db_lock().await;
        let pool = test_pool().await;
        let http_client = crate::http::new_client();
        let ctx = IndexerContext {
            pool: &pool,
            http_client: &http_client,
        };
        let resolver = DefaultIndexerResolver::new(Default::default());

        // Clean up all existing sources
        let existing = watch_repo::list_enabled_sources(&pool)
            .await
            .expect("list sources");
        for source in existing {
            let _ = watch_repo::delete_indexer_source(&pool, source.id).await;
        }

        // With no native sources, falls back to Prowlarr (which will fail in test)
        let result = search_indexers(&SearchIndexersParams {
            ctx: &ctx,
            indexers: &resolver,
            pool: &pool,
            query: "test",
            limit: 10,
            external_ids: &[],
            media_want: None,
            media_type_id: None,
            per_source_timeout: None,
            season: None,
            episode: None,
        })
        .await;

        // Should either error (Prowlarr not running) or return empty
        assert!(
            result.is_err() || result.unwrap().is_empty(),
            "expected error or empty results with no sources"
        );
    }
}
