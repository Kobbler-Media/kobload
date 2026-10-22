//! TvSeason episode fallback logic.
//!
//! When a season pack search fails, this module provides:
//! - Phase 2: Pluck individual episodes from existing search results
//! - Phase 3: Search for remaining episodes individually

use std::collections::{HashMap, HashSet};

use tracing::{info, warn};
use uuid::Uuid;

use super::scoring::SearchContext as ScoringSearchContext;
use crate::helpers::{
    release_matches_categories, resolve_categories_for_media_type, resolve_categories_for_source,
};
use crate::indexers::{IndexerContext, IndexerResolver, IndexerSearchRequest};
use crate::media::{episode_match_target, episode_search_terms, xem_match_targets};
use crate::repositories::quality_profiles::QualityProfileRow;
use crate::repositories::watch as watch_repo;
use crate::services::search::search_type_from_media_want;
use crate::watch::{ExternalId, MatchTarget, ProcessedRelease, Release};
use nats_jobs::{EpisodeRef, MediaWant, QualityOverrides};
use tracing::debug;

use super::downloads::create_download;
use super::scoring::{ScoringConfig, score_batch};
use super::types::WantServiceContext;

/// Result of the episode fallback process.
pub(super) struct EpisodeFallbackResult {
    /// Number of episodes downloaded.
    pub episodes_downloaded: Vec<String>,
    /// Episodes still missing after all phases.
    pub episodes_missing: Vec<String>,
    /// First download ID created (for reporting).
    pub first_download_id: Option<Uuid>,
    /// First selected release (for reporting).
    pub first_release: Option<ProcessedRelease>,
}

/// Run the episode fallback for a TvSeason want.
///
/// Phase 2: Pluck episodes from `existing_releases` (already fetched by season pack search).
/// Phase 3: Search individually for any gaps.
#[allow(clippy::too_many_arguments)]
pub(super) async fn run_episode_fallback(
    ctx: &WantServiceContext<'_>,
    show_name: &str,
    season: &str,
    episodes_needed: &[EpisodeRef],
    existing_releases: &[Release],
    media_want: &MediaWant,
    min_score: f64,
    callback_url: &str,
    auth_token: &Option<String>,
    metadata: &Option<serde_json::Value>,
    quality_profile: Option<&QualityProfileRow>,
    quality_overrides: Option<&QualityOverrides>,
    indexers: &dyn IndexerResolver,
    external_ids: &[ExternalId],
    media_type_id: Option<Uuid>,
    limit: u32,
    watch_pattern_id: Option<Uuid>,
    preferred_protocol: Option<&str>,
    upgrade_ctx: &super::EpisodeUpgradeContext,
) -> Result<EpisodeFallbackResult, String> {
    // Filter out unaired episodes — never search for episodes that haven't aired yet.
    // Once we hit an episode with a future air_date, stop — subsequent episodes
    // (including those with no air_date) are presumed to not yet be available.
    // Sort by sort_order first (episodes_needed may be string-sorted: "1","10","2"...).
    let today = time::OffsetDateTime::now_utc().date();
    let mut sorted_eps: Vec<&EpisodeRef> = episodes_needed.iter().collect();
    sorted_eps.sort_by_key(|ep| {
        ep.sort_order
            .unwrap_or(ep.episode.parse::<i32>().unwrap_or(999))
    });
    let mut hit_future = false;
    let aired_episodes: Vec<&EpisodeRef> = sorted_eps
        .into_iter()
        .filter(|ep| {
            if hit_future {
                return false;
            }
            if let Some(d) = ep
                .air_date
                .as_deref()
                .and_then(super::scoring::parse_iso_date)
                && d > today
            {
                hit_future = true;
                return false;
            }
            true
        })
        .collect();

    let needed_set: HashSet<String> = aired_episodes.iter().map(|e| e.episode.clone()).collect();
    let mut covered: HashSet<String> = HashSet::new();
    let mut first_download_id = None;
    let mut first_release = None;
    let media_want_json = serde_json::to_value(media_want).ok();

    // Resolve allowed categories for post-filtering
    let allowed_categories = if let Some(mt_id) = media_type_id {
        resolve_categories_for_media_type(ctx.pool, Some(mt_id))
            .await
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    let xem_show_map = ctx
        .xem_cache
        .show_map_for_want(ctx.http_client, ctx.pool, media_want)
        .await;

    // Phase 2: Pluck from existing results (aired episodes only)
    let episode_targets: Vec<MatchTarget> = aired_episodes
        .iter()
        .map(|ep| episode_match_target(show_name, season, &ep.episode))
        .collect();

    if !episode_targets.is_empty() {
        // Post-filter existing releases by category
        let filtered_existing: Vec<&Release> = existing_releases
            .iter()
            .filter(|r| release_matches_categories(&r.category_ids, &allowed_categories))
            .collect();
        let filtered_refs: Vec<Release> = filtered_existing.into_iter().cloned().collect();
        if filtered_refs.len() < existing_releases.len() {
            debug!(
                before = existing_releases.len(),
                after = filtered_refs.len(),
                "Phase 2: filtered existing releases by category"
            );
        }

        let mut config = ScoringConfig::new(
            &episode_targets,
            &[],
            min_score,
            quality_profile,
            ctx.trash_filters.as_deref(),
        )
        .with_media_want(media_want)
        .with_search_context(ScoringSearchContext::TextFallback);
        if let Some(overrides) = quality_overrides {
            config = config.with_quality_overrides(overrides);
        }
        if let Some(ref xem) = xem_show_map {
            config = config.with_xem_show_map(xem);
        }
        let batch = score_batch(&filtered_refs, &config);
        let candidates: Vec<ProcessedRelease> =
            batch.into_iter().filter(|pr| pr.is_candidate()).collect();

        // Group by matched_target and download best per episode
        let mut by_episode: HashMap<Option<nats_jobs::MatchedTarget>, Vec<&ProcessedRelease>> =
            HashMap::new();
        for result in &candidates {
            by_episode
                .entry(result.matched_target.clone())
                .or_default()
                .push(result);
        }

        for (matched_target, mut results) in by_episode {
            results.sort_by(|a, b| {
                b.total_score
                    .partial_cmp(&a.total_score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });

            // Apply protocol preference: prefer preferred protocol among eligible results
            let best_candidate = {
                let eligible: Vec<&&ProcessedRelease> = results
                    .iter()
                    .filter(|r| r.match_score.unwrap_or(0.0) >= min_score)
                    .collect();
                if let Some(pref) = preferred_protocol
                    && let Some(preferred_result) =
                        eligible.iter().find(|r| r.release.protocol == pref)
                {
                    Some(**preferred_result)
                } else {
                    eligible.into_iter().next().copied()
                }
            };

            if let Some(best) = best_candidate {
                // Extract episode identifier from the MatchedTarget
                let ep_id = match &matched_target {
                    Some(nats_jobs::MatchedTarget::Episode { episode, .. }) => episode.clone(),
                    Some(nats_jobs::MatchedTarget::SceneEpisode { tvdb_episode, .. }) => {
                        tvdb_episode.to_string()
                    }
                    _ => continue, // skip non-episode targets
                };

                // Cross-validate: extract episode key from the actual release title
                // and verify it's in episodes_needed. Prevents downloading the wrong
                // episode when the fuzzy scorer matched against the wrong target.
                if let Some(ref mw_json) = media_want_json
                    && let Some(actual_key) =
                        crate::watch::episode_key::extract_episode_key(&best.release.title, mw_json)
                    && !crate::watch::episode_key::is_episode_needed(&actual_key, episodes_needed)
                {
                    debug!(
                        release = %best.release.title,
                        actual_key = %actual_key,
                        matched_target = %ep_id,
                        "Phase 2: skipping - release episode not in episodes_needed"
                    );
                    continue;
                }

                // Reserve match first to prevent concurrent duplicate downloads
                let match_id = reserve_episode_match(
                    ctx.pool,
                    watch_pattern_id,
                    best,
                    Some(season),
                    media_want_json.as_ref(),
                    upgrade_ctx.manual,
                )
                .await;

                let Some(match_id) = match_id else {
                    debug!(
                        episode = %ep_id,
                        release = %best.release.title,
                        "Phase 2: race avoided, match already exists"
                    );
                    continue;
                };

                // Episode upgrade/library check
                let ep_key_str = format!(
                    "S{:02}E{:02}",
                    season.parse::<u32>().unwrap_or(1),
                    ep_id.parse::<u32>().unwrap_or(0)
                );
                if let Some(reason) = super::should_skip_episode(
                    ctx.pool,
                    &ep_key_str,
                    &best.release.title,
                    Some(&best.release.publish_date),
                    upgrade_ctx,
                    quality_profile,
                    ctx.trash_filters.as_ref(),
                )
                .await
                {
                    if let Some(mid) = match_id {
                        let _ = watch_repo::update_match_skipped(ctx.pool, mid, &reason).await;
                    }
                    continue;
                }

                match create_download(
                    ctx,
                    best,
                    callback_url,
                    auth_token,
                    metadata,
                    crate::media::parse_media_kind(media_want),
                )
                .await
                {
                    Ok(id) => {
                        if let Some(mid) = match_id {
                            let _ = watch_repo::update_match_download_id(ctx.pool, mid, id).await;
                        }
                        covered.insert(ep_id.clone());
                        if first_download_id.is_none() {
                            first_download_id = Some(id);
                            first_release = Some(best.clone());
                        }
                        info!(episode = %ep_id, "Phase 2: downloaded episode from existing results");
                    }
                    Err(e) => {
                        warn!(episode = %ep_id, error = %e, "Phase 2: failed to create download");
                        if let Some(mid) = match_id {
                            let reason = format!("create_download failed: {e}");
                            let _ = watch_repo::update_match_skipped(ctx.pool, mid, &reason).await;
                        }
                    }
                }
            }
        }
    }

    // Phase 3: Search for remaining gaps (aired episodes only)
    let missing: Vec<&&EpisodeRef> = aired_episodes
        .iter()
        .filter(|ep| !covered.contains(&ep.episode))
        .collect();

    if !missing.is_empty() {
        info!(
            missing_count = missing.len(),
            "Phase 3: searching for individual episodes"
        );

        let indexer_ctx = IndexerContext {
            pool: ctx.pool,
            http_client: ctx.http_client,
        };

        for ep_ref in &missing {
            let mut terms = episode_search_terms(show_name, season, &ep_ref.episode);
            let target = episode_match_target(show_name, season, &ep_ref.episode);
            let mut targets = vec![target];

            // Augment with XEM scene numbering if available
            if let Some(ref show_map) = xem_show_map
                && let (Ok(tvdb_s), Ok(tvdb_e)) =
                    (season.parse::<u32>(), ep_ref.episode.parse::<u32>())
                && let Some(scene) = show_map.scene_for_tvdb(tvdb_s, tvdb_e)
                && (scene.scene_season != tvdb_s || scene.scene_episode != tvdb_e)
            {
                let extra_terms = episode_search_terms(
                    show_name,
                    &scene.scene_season.to_string(),
                    &scene.scene_episode.to_string(),
                );
                for term in extra_terms {
                    if !terms.contains(&term) {
                        terms.push(term);
                    }
                }
                targets.extend(xem_match_targets(
                    show_name,
                    scene.scene_season,
                    scene.scene_episode,
                    tvdb_s,
                    tvdb_e,
                ));
            }

            let mut ep_releases = Vec::new();
            let mut seen_guids: HashSet<String> = HashSet::new();

            // Determine season/episode params for structured search
            let (s_param, e_param) = structured_params(season, &ep_ref.episode);

            for term in &terms {
                let releases = search_for_episode(
                    &indexer_ctx,
                    indexers,
                    ctx.pool,
                    term,
                    limit,
                    external_ids,
                    Some(media_want),
                    media_type_id,
                    s_param.as_deref(),
                    e_param.as_deref(),
                )
                .await?;
                for r in releases {
                    if seen_guids.insert(r.guid.clone()) {
                        ep_releases.push(r);
                    }
                }
            }

            // Post-filter by category
            ep_releases
                .retain(|r| release_matches_categories(&r.category_ids, &allowed_categories));

            let mut ep_config = ScoringConfig::new(
                &targets,
                &[],
                min_score,
                quality_profile,
                ctx.trash_filters.as_deref(),
            )
            .with_media_want(media_want)
            .with_search_context(ScoringSearchContext::TextFallback);
            if let Some(overrides) = quality_overrides {
                ep_config = ep_config.with_quality_overrides(overrides);
            }
            if let Some(ref xem) = xem_show_map {
                ep_config = ep_config.with_xem_show_map(xem);
            }
            let ep_batch = score_batch(&ep_releases, &ep_config);
            let mut candidates: Vec<ProcessedRelease> = ep_batch
                .into_iter()
                .filter(|pr| pr.is_candidate())
                .collect();

            // Apply protocol preference when selecting best episode result
            candidates.sort_by(|a, b| {
                b.total_score
                    .partial_cmp(&a.total_score)
                    .unwrap_or(std::cmp::Ordering::Equal)
            });
            let best_for_ep = {
                let eligible: Vec<&ProcessedRelease> = candidates
                    .iter()
                    .filter(|r| r.match_score.unwrap_or(0.0) >= min_score)
                    .collect();
                if let Some(pref) = preferred_protocol
                    && let Some(preferred_result) =
                        eligible.iter().find(|r| r.release.protocol == pref)
                {
                    Some(*preferred_result)
                } else {
                    eligible.into_iter().next()
                }
            };

            if let Some(best) = best_for_ep {
                // Cross-validate episode key from release title
                if let Some(ref mw_json) = media_want_json
                    && let Some(actual_key) =
                        crate::watch::episode_key::extract_episode_key(&best.release.title, mw_json)
                    && !crate::watch::episode_key::is_episode_needed(&actual_key, episodes_needed)
                {
                    debug!(
                        release = %best.release.title,
                        actual_key = %actual_key,
                        episode = %ep_ref.episode,
                        "Phase 3: skipping - release episode not in episodes_needed"
                    );
                    continue;
                }

                // Reserve match first to prevent concurrent duplicate downloads
                let match_id = reserve_episode_match(
                    ctx.pool,
                    watch_pattern_id,
                    best,
                    Some(season),
                    media_want_json.as_ref(),
                    upgrade_ctx.manual,
                )
                .await;

                let Some(match_id) = match_id else {
                    debug!(
                        episode = %ep_ref.episode,
                        release = %best.release.title,
                        "Phase 3: race avoided, match already exists"
                    );
                    continue;
                };

                // Episode upgrade/library check
                let ep_key_str = format!(
                    "S{:02}E{:02}",
                    season.parse::<u32>().unwrap_or(1),
                    ep_ref.episode.parse::<u32>().unwrap_or(0)
                );
                if let Some(reason) = super::should_skip_episode(
                    ctx.pool,
                    &ep_key_str,
                    &best.release.title,
                    Some(&best.release.publish_date),
                    upgrade_ctx,
                    quality_profile,
                    ctx.trash_filters.as_ref(),
                )
                .await
                {
                    if let Some(mid) = match_id {
                        let _ = watch_repo::update_match_skipped(ctx.pool, mid, &reason).await;
                    }
                    continue;
                }

                match create_download(
                    ctx,
                    best,
                    callback_url,
                    auth_token,
                    metadata,
                    crate::media::parse_media_kind(media_want),
                )
                .await
                {
                    Ok(id) => {
                        if let Some(mid) = match_id {
                            let _ = watch_repo::update_match_download_id(ctx.pool, mid, id).await;
                        }
                        covered.insert(ep_ref.episode.clone());
                        if first_download_id.is_none() {
                            first_download_id = Some(id);
                            first_release = Some(best.clone());
                        }
                        info!(episode = %ep_ref.episode, "Phase 3: downloaded episode from individual search");
                    }
                    Err(e) => {
                        warn!(episode = %ep_ref.episode, error = %e, "Phase 3: failed to create download");
                        if let Some(mid) = match_id {
                            let reason = format!("create_download failed: {e}");
                            let _ = watch_repo::update_match_skipped(ctx.pool, mid, &reason).await;
                        }
                    }
                }
            }
        }
    }

    let episodes_downloaded: Vec<String> = needed_set
        .iter()
        .filter(|e| covered.contains(*e))
        .cloned()
        .collect();
    let episodes_missing: Vec<String> = needed_set
        .iter()
        .filter(|e| !covered.contains(*e))
        .cloned()
        .collect();

    Ok(EpisodeFallbackResult {
        episodes_downloaded,
        episodes_missing,
        first_download_id,
        first_release,
    })
}

/// Determine structured Newznab season/episode params from string identifiers.
fn structured_params(season: &str, episode: &str) -> (Option<String>, Option<String>) {
    let s = season.parse::<u32>().ok().map(|n| n.to_string());
    let e = episode.parse::<u32>().ok().map(|n| n.to_string());
    (s, e)
}

/// Search indexers for a specific episode.
#[allow(clippy::too_many_arguments)]
async fn search_for_episode(
    ctx: &IndexerContext<'_>,
    indexers: &dyn IndexerResolver,
    pool: &sqlx::PgPool,
    query: &str,
    _limit: u32,
    external_ids: &[ExternalId],
    media_want: Option<&MediaWant>,
    media_type_id: Option<Uuid>,
    season: Option<&str>,
    episode: Option<&str>,
) -> Result<Vec<Release>, String> {
    let native_handler_types: HashSet<&str> =
        ["newznab", "torznab", "cardigann"].into_iter().collect();
    let search_type = search_type_from_media_want(media_want);

    let enabled_sources = watch_repo::list_enabled_sources(pool)
        .await
        .map_err(|e| e.to_string())?;

    let native_sources: Vec<_> = enabled_sources
        .iter()
        .filter(|s| native_handler_types.contains(s.handler_type.as_str()))
        .filter(|s| {
            if let Some(mt_id) = media_type_id {
                s.media_type_ids.is_empty() || s.media_type_ids.contains(&mt_id)
            } else {
                true
            }
        })
        .collect();

    let mut releases = Vec::new();

    for source in &native_sources {
        if watch_repo::should_skip_source(source) {
            continue;
        }

        let handler = match indexers.get(&source.handler_type) {
            Some(h) => h,
            None => continue,
        };

        let search_categories = resolve_categories_for_source(pool, source, media_type_id)
            .await
            .map_err(|e| format!("Category resolution failed: {e}"))?;

        match handler
            .search(
                ctx,
                Some(source),
                IndexerSearchRequest {
                    query: query.to_string(),
                    categories: search_categories,
                    limit: None, // Don't send limit — let indexer use its default (matches Sonarr)
                    offset: None,
                    external_ids: external_ids.to_vec(),
                    search_type,
                    season: season.map(|s| s.to_string()),
                    episode: episode.map(|e| e.to_string()),
                },
            )
            .await
        {
            Ok(mut source_releases) => {
                let _ = watch_repo::record_success(pool, source.id).await;
                releases.append(&mut source_releases);
            }
            Err(err) => {
                crate::indexers::record_error(pool, source.id, &err).await;
                warn!(error = %err, source = %source.name, "Episode search failed");
            }
        }
    }

    Ok(releases)
}

/// Compute episode key from season + media_want context and delegate to shared reserve_watch_match.
async fn reserve_episode_match(
    pool: &sqlx::PgPool,
    watch_pattern_id: Option<Uuid>,
    best: &ProcessedRelease,
    season: Option<&str>,
    media_want_json: Option<&serde_json::Value>,
    manual: bool,
) -> Option<Option<Uuid>> {
    // Primary: extract episode key from the actual release title (authoritative).
    // Fallback: derive from matched_target (may be wrong if fuzzy scorer
    // matched against the wrong episode target).
    let episode_key = season.and_then(|s| {
        if let Some(mw) = media_want_json
            && let Some(key) =
                crate::watch::episode_key::extract_episode_key(&best.release.title, mw)
        {
            return Some(key);
        }
        let s_num: u32 = s.parse().ok()?;
        match &best.matched_target {
            Some(nats_jobs::MatchedTarget::Episode { episode, .. }) => {
                let ep: u32 = episode.parse().ok()?;
                Some(format!("S{:02}E{:02}", s_num, ep))
            }
            Some(nats_jobs::MatchedTarget::SceneEpisode { tvdb_episode, .. }) => {
                Some(format!("S{:02}E{:02}", s_num, tvdb_episode))
            }
            _ => None,
        }
    });
    super::reserve_watch_match(pool, watch_pattern_id, best, episode_key.as_deref(), manual).await
}
