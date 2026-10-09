//! Protocol delay hold logic for watch matches.
//!
//! When a media type config has `protocol_priority` configured, lower-priority protocol
//! matches are held for `protocol_delay_minutes` to allow a higher-priority
//! match to appear. If a higher-priority match arrives, the held match is
//! superseded. When the hold expires, a final search is attempted before
//! grabbing the held release.

use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::helpers::resolve_categories_for_media_type;
use crate::media::search_terms_from_media_want;
use crate::repositories::{media_types, watch as watch_repo};
use crate::services::downloads::{
    DownloadServiceContext, DownloadSubmission, DownloadUrlContext, submit_download,
};
use crate::services::protocol::resolve_protocol_config;
use crate::state::AppState;
use crate::watch::episode_key;
use crate::watch::service::get_pattern_download_state;

use super::types::Release;

/// Determine if a release should be held due to protocol priority.
/// Returns `Some(hold_until)` if the release should be held, `None` if it should be grabbed.
pub async fn should_hold_for_protocol(
    state: &AppState,
    pattern: &watch_repo::WatchPatternRow,
    release: &Release,
) -> Option<time::OffsetDateTime> {
    let config = resolve_protocol_config(&state.pool, pattern.media_type_id).await?;

    if config.delay_minutes <= 0 {
        return None;
    }

    let effective_protocol = effective_protocol_for_release(&state.pool, release).await;
    let top_protocol = config.preferred()?;
    if effective_protocol == top_protocol {
        return None; // Top priority -- grab immediately
    }

    let hold_until =
        time::OffsetDateTime::now_utc() + time::Duration::minutes(config.delay_minutes as i64);
    Some(hold_until)
}

/// Resolve the effective protocol for a release, taking debrid_mode into account.
/// Releases from debrid sources are treated as protocol "debrid" for priority/hold
/// purposes, even though their wire protocol is "torrent".
///
/// Detection: either the source has `debrid_mode='always'`, or the release came
/// from the built-in hash index (indexer starts with "hash_index:").
async fn effective_protocol_for_release(pool: &sqlx::PgPool, release: &Release) -> String {
    // Built-in hash index releases (no source row)
    if release.indexer.starts_with("hash_index:") {
        return "debrid".to_string();
    }
    // Configured sources with debrid_mode
    if let Some(source_id) = release.indexer_source_id
        && let Ok(Some(source)) = watch_repo::get_indexer_source_by_id(pool, source_id).await
        && source.debrid_mode == "always"
    {
        return "debrid".to_string();
    }
    release.protocol.clone()
}

/// If we found a higher-priority match, supersede any existing held matches.
pub async fn check_and_supersede_holds(
    state: &AppState,
    pattern: &watch_repo::WatchPatternRow,
    release: &Release,
) {
    let Some(config) = resolve_protocol_config(&state.pool, pattern.media_type_id).await else {
        return;
    };

    let effective = effective_protocol_for_release(&state.pool, release).await;
    let release_rank = config.priority.iter().position(|p| p == &effective);

    let held = match watch_repo::get_held_matches_for_pattern(&state.pool, pattern.id).await {
        Ok(h) => h,
        Err(_) => return,
    };

    // For TvSeason patterns, only supersede holds with the same episode_key.
    let new_episode_key = pattern.media_want.as_ref().and_then(|mw| {
        if episode_key::is_tv_season(mw) {
            episode_key::extract_episode_key(&release.title, mw)
        } else {
            None
        }
    });

    for held_match in held {
        let held_protocol = held_match
            .release_data
            .as_ref()
            .and_then(|d| d.get("protocol"))
            .and_then(|p| p.as_str())
            .unwrap_or("");

        let held_rank = config.priority.iter().position(|p| p == held_protocol);

        if let (Some(new_rank), Some(old_rank)) = (release_rank, held_rank)
            && new_rank < old_rank
        {
            // For TvSeason, only supersede if episode keys match (or either is None for compat)
            if let Some(ref new_ek) = new_episode_key {
                let held_ek = held_match.episode_key.clone().or_else(|| {
                    pattern.media_want.as_ref().and_then(|mw| {
                        episode_key::extract_episode_key(&held_match.release_title, mw)
                    })
                });
                if let Some(ref h_ek) = held_ek
                    && h_ek != new_ek
                {
                    continue; // Different episodes, don't supersede
                }
                // If held has no episode_key, supersede anyway (backwards compat)
            }

            let _ = watch_repo::discard_hold(&state.pool, held_match.id).await;
            info!(
                match_id = %held_match.id,
                old_protocol = held_protocol,
                new_protocol = %release.protocol,
                "Superseded held match with higher-priority protocol"
            );
        }
    }
}

/// Process held matches whose hold_until has expired.
///
/// Before grabbing the held release, performs a final search to check if
/// a preferred-protocol release has become available since the hold was created.
pub async fn process_expired_holds(state: &AppState) {
    let expired = match watch_repo::get_expired_holds(&state.pool).await {
        Ok(h) => h,
        Err(e) => {
            error!(error = %e, "Failed to fetch expired holds");
            return;
        }
    };

    for held_match in expired {
        let pattern = match watch_repo::get_pattern_by_id(&state.pool, held_match.watch_pattern_id)
            .await
        {
            Ok(Some(p)) => p,
            _ => {
                let _ = watch_repo::release_hold(&state.pool, held_match.id, "pattern_gone", None)
                    .await;
                continue;
            }
        };

        // Check if pattern already has an active or imported download.
        // For TvSeason patterns with an episode_key, use per-episode checks.
        let use_episode_check = held_match.episode_key.as_ref().and_then(|ek| {
            pattern
                .media_want
                .as_ref()
                .filter(|mw| episode_key::is_tv_season(mw))
                .map(|_| ek.clone())
        });

        if let Some(ref ek) = use_episode_check {
            let ep_has_active =
                watch_repo::episode_has_active_download(&state.pool, pattern.id, ek)
                    .await
                    .unwrap_or(false);
            if ep_has_active {
                debug!(
                    match_id = %held_match.id,
                    pattern_id = %pattern.id,
                    episode_key = %ek,
                    "Skipping held match release - episode already has active download"
                );
                let _ = watch_repo::release_hold(
                    &state.pool,
                    held_match.id,
                    "skipped_active_download",
                    None,
                )
                .await;
                continue;
            }
            let ep_has_imported =
                watch_repo::episode_has_imported_download(&state.pool, pattern.id, ek)
                    .await
                    .unwrap_or(false);
            if ep_has_imported {
                debug!(
                    match_id = %held_match.id,
                    pattern_id = %pattern.id,
                    episode_key = %ek,
                    "Skipping held match release - episode already has imported download"
                );
                let _ = watch_repo::release_hold(
                    &state.pool,
                    held_match.id,
                    "skipped_already_imported",
                    None,
                )
                .await;
                continue;
            }
        } else {
            let dl_state = get_pattern_download_state(&state.pool, pattern.id).await;
            if dl_state.has_active {
                debug!(
                    match_id = %held_match.id,
                    pattern_id = %pattern.id,
                    "Skipping held match release - pattern already has active download"
                );
                let _ = watch_repo::release_hold(
                    &state.pool,
                    held_match.id,
                    "skipped_active_download",
                    None,
                )
                .await;
                continue;
            }
            if dl_state.has_imported {
                debug!(
                    match_id = %held_match.id,
                    pattern_id = %pattern.id,
                    "Skipping held match release - pattern already has imported download"
                );
                let _ = watch_repo::release_hold(
                    &state.pool,
                    held_match.id,
                    "skipped_already_imported",
                    None,
                )
                .await;
                continue;
            }
        }

        // Final search: check if a preferred-protocol release is now available
        if let Some(candidate) = final_search_for_hold(state, &pattern, &held_match).await {
            let dl_ctx = DownloadServiceContext {
                pool: &state.pool,
                http_client: &state.http_client,
                download_events: &state.download_events,
                download_clients: state.download_clients.as_ref(),
                indexers: state.indexers.as_ref(),
            };

            let metadata = serde_json::json!({
                "watch_pattern_id": pattern.id,
                "source_system": pattern.source_system,
                "source_id": pattern.source_id,
                "original_metadata": pattern.metadata,
                "final_search_upgrade": true,
            });

            let media_kind = resolve_media_kind(&state.pool, &pattern).await;

            let download_url = candidate
                .release
                .download_url
                .clone()
                .or_else(|| candidate.release.magnet_url.clone())
                .unwrap_or_default();
            let submission = DownloadSubmission {
                source: "watch".to_string(),
                protocol: candidate.release.protocol.clone(),
                name: candidate.release.title.clone(),
                download_url,
                download_client_id: None,
                callback_url: Some(pattern.callback_url.clone()),
                auth_token: None,
                metadata,
                release_payload: Some(candidate.release.raw_data.clone()),
                api_key_id: None,
                url_context: DownloadUrlContext::default(),
                indexer_source_id: candidate.release.indexer_source_id,
                quality_profile_id: None,
                media_kind,
                skip_duplicate_url: true,
            };

            match submit_download(&dl_ctx, submission).await {
                Ok(outcome) => {
                    // Create a watch_match for the replacement so dedup tracks it
                    let episode_key = held_match.episode_key.clone().or_else(|| {
                        pattern.media_want.as_ref().and_then(|mw| {
                            episode_key::extract_episode_key(&candidate.release.title, mw)
                        })
                    });
                    let match_input = watch_repo::WatchMatchInput {
                        pattern_id: pattern.id,
                        indexer_source_id: candidate.release.indexer_source_id,
                        release_guid: &candidate.release.guid,
                        release_title: &candidate.release.title,
                        release_data: Some(&candidate.release.raw_data),
                        match_score: candidate.total_score,
                        matched_pattern: candidate.matched_pattern.as_deref().unwrap_or(""),
                        action: "downloaded",
                        download_id: Some(outcome.id),
                        hold_until: None,
                        held: false,
                        quality_score: candidate.quality_score,
                        rejected: false,
                        rejected_at: None,
                        rejected_reason: None,
                        episode_key: episode_key.as_deref(),
                    };
                    if let Err(e) =
                        watch_repo::create_match_if_not_exists(&state.pool, &match_input).await
                    {
                        warn!(
                            error = %e,
                            match_id = %held_match.id,
                            "Failed to create watch_match for final search replacement"
                        );
                    }

                    let _ = watch_repo::release_hold(
                        &state.pool,
                        held_match.id,
                        "superseded_final_search",
                        Some(outcome.id),
                    )
                    .await;
                    // Cancel any other held matches for this pattern+episode
                    let _ = watch_repo::cancel_held_matches_for_pattern(
                        &state.pool,
                        held_match.watch_pattern_id,
                        held_match.episode_key.as_deref(),
                        held_match.id,
                    )
                    .await;
                    info!(
                        match_id = %held_match.id,
                        download_id = %outcome.id,
                        title = %candidate.release.title,
                        protocol = %candidate.release.protocol,
                        "Final search found better match, superseded held release"
                    );
                    continue;
                }
                Err(e) => {
                    warn!(
                        error = %e,
                        match_id = %held_match.id,
                        "Final search download failed, falling back to held release"
                    );
                    // Fall through to grab the held release
                }
            }
        }

        // Grab the original held release
        grab_held_release(state, &held_match, &pattern).await;
    }
}

/// Grab the originally-held release when the hold expires or by user request.
pub(crate) async fn grab_held_release(
    state: &AppState,
    held_match: &watch_repo::WatchMatchRow,
    pattern: &watch_repo::WatchPatternRow,
) {
    // Re-check quality against current profile before downloading.
    // The profile may have changed since the match was held (e.g., user added
    // "ts" to rejected_sources after a TeleSync was held).
    if let Some(mt_id) = pattern.media_type_id {
        let profile = crate::watch::service::resolve_quality_profile(
            &state.pool,
            mt_id,
            &mut std::collections::HashMap::new(),
        )
        .await;
        let trash_filters = state.trash_filters.get_filters();
        let quality = crate::quality::score_quality(
            &held_match.release_title,
            profile.as_ref(),
            None,
            trash_filters.as_deref(),
        );
        if quality.rejected {
            warn!(
                match_id = %held_match.id,
                release = %held_match.release_title,
                reason = ?quality.primary_reject_reason(),
                "Held release quality-rejected by current profile, skipping"
            );
            let _ = watch_repo::release_hold(&state.pool, held_match.id, "quality_rejected", None)
                .await;
            return;
        }
    }

    let release_data = held_match.release_data.clone().unwrap_or_default();
    let download_url = release_data
        .get("download_url")
        .and_then(|v| v.as_str())
        .map(String::from)
        .or_else(|| {
            release_data
                .get("magnet_url")
                .and_then(|v| v.as_str())
                .map(String::from)
        });

    let Some(download_url) = download_url else {
        let _ = watch_repo::release_hold(&state.pool, held_match.id, "no_url", None).await;
        return;
    };

    let protocol = release_data
        .get("protocol")
        .and_then(|v| v.as_str())
        .unwrap_or("torrent")
        .to_string();

    let metadata = serde_json::json!({
        "watch_pattern_id": pattern.id,
        "source_system": pattern.source_system,
        "source_id": pattern.source_id,
        "original_metadata": pattern.metadata,
        "held_match": true,
    });

    let media_kind = resolve_media_kind(&state.pool, pattern).await;

    let submission = DownloadSubmission {
        source: "watch".to_string(),
        protocol,
        name: held_match.release_title.clone(),
        download_url,
        download_client_id: None,
        callback_url: Some(pattern.callback_url.clone()),
        auth_token: None,
        metadata,
        release_payload: Some(release_data.clone()),
        api_key_id: None,
        url_context: DownloadUrlContext::default(),
        indexer_source_id: release_data
            .get("indexer_source_id")
            .and_then(|v| v.as_str())
            .and_then(|v| uuid::Uuid::parse_str(v).ok()),
        quality_profile_id: None,
        media_kind,
        skip_duplicate_url: true,
    };

    let ctx = DownloadServiceContext {
        pool: &state.pool,
        http_client: &state.http_client,
        download_events: &state.download_events,
        download_clients: state.download_clients.as_ref(),
        indexers: state.indexers.as_ref(),
    };

    match submit_download(&ctx, submission).await {
        Ok(outcome) => {
            let _ = watch_repo::release_hold(
                &state.pool,
                held_match.id,
                "downloaded",
                Some(outcome.id),
            )
            .await;
            // Cancel any other held matches for this pattern+episode
            let _ = watch_repo::cancel_held_matches_for_pattern(
                &state.pool,
                held_match.watch_pattern_id,
                held_match.episode_key.as_deref(),
                held_match.id,
            )
            .await;
            info!(
                match_id = %held_match.id,
                download_id = %outcome.id,
                "Released held match after protocol delay expired"
            );
        }
        Err(e) => {
            error!(error = %e, match_id = %held_match.id, "Failed to create download for expired hold");
            let _ =
                watch_repo::release_hold(&state.pool, held_match.id, "download_failed", None).await;
        }
    }
}

/// Whether a final-search candidate is an acceptable replacement for the held
/// release, in terms of episode coverage.
///
/// The final search exists to swap one held release for a better one, so the
/// candidate has to cover the same episode. A season pack nominally "covers"
/// the held episode, but grabbing a whole season to replace a single episode is
/// never the intent — and for a still-airing season it pulls unaired episodes
/// too. Packs are therefore only accepted when the held release was itself a
/// pack.
///
/// Patterns without an episode key (non-TvSeason, or an unparseable title) are
/// left unconstrained: there is no episode to compare against.
fn replacement_covers_held_episode(
    held_match: &watch_repo::WatchMatchRow,
    media_want: &Option<serde_json::Value>,
    candidate: &crate::watch::ProcessedRelease,
) -> bool {
    let Some(held_key) = held_match.episode_key.as_deref() else {
        return true;
    };
    let Some(mw_json) = media_want.as_ref() else {
        return true;
    };
    let Some(candidate_key) = episode_key::extract_episode_key(&candidate.release.title, mw_json)
    else {
        return true;
    };

    // Only a pack may replace a pack.
    if candidate_key == episode_key::SEASON_PACK_KEY {
        return held_key == episode_key::SEASON_PACK_KEY;
    }

    // Otherwise the candidate must cover every episode the held release did.
    // An unparseable held key yields no episodes; treat that as unconstrained
    // rather than vacuously satisfied.
    let held_episodes = episode_key::episodes_from_key(held_key);
    if held_episodes.is_empty() {
        return true;
    }
    held_episodes
        .iter()
        .all(|ep| episode_key::key_covers_episode(&candidate_key, ep))
}

/// Perform a final search before releasing a held match.
///
/// Returns `Some(candidate)` if a better result was found (preferred protocol,
/// or higher score than the held release). Returns `None` to grab the held release.
async fn final_search_for_hold(
    state: &AppState,
    pattern: &watch_repo::WatchPatternRow,
    held_match: &watch_repo::WatchMatchRow,
) -> Option<crate::watch::ProcessedRelease> {
    use crate::services::search::{
        SearchContext, SearchServiceContext, SearchServiceRequest, search,
    };

    // Parse the pattern's MediaWant once — it drives both the search terms and
    // the structural gating below.
    let parsed_media_want = pattern
        .media_want
        .as_ref()
        .and_then(|mw| serde_json::from_value::<nats_jobs::MediaWant>(mw.clone()).ok());

    // Build search terms from the pattern
    let search_terms = if !pattern.patterns.is_empty() {
        pattern.patterns.clone()
    } else if let Some(ref mw) = parsed_media_want {
        search_terms_from_media_want(mw)
    } else {
        return None;
    };

    let query = search_terms.first()?.clone();
    if query.is_empty() {
        return None;
    }

    debug!(
        match_id = %held_match.id,
        pattern_id = %pattern.id,
        query = %query,
        "Running final search before releasing held match"
    );

    let request = SearchServiceRequest {
        query,
        limit: None, // Don't send limit — let indexer use its default (matches Sonarr)
        offset: None,
        media_type_id: pattern.media_type_id,
        protocol: None,
        media_want: None,
        external_ids: Vec::new(),
        search_context: SearchContext::Interactive,
    };

    let ctx = SearchServiceContext {
        pool: &state.pool,
        http_client: &state.http_client,
        indexers: state.indexers.as_ref(),
    };

    let search_result = match search(&ctx, request, None, None).await {
        Ok(r) => r,
        Err(e) => {
            warn!(
                error = %e,
                match_id = %held_match.id,
                "Final search failed, will grab held release"
            );
            return None;
        }
    };

    if search_result.results.is_empty() {
        debug!(
            match_id = %held_match.id,
            "Final search returned no results"
        );
        return None;
    }

    // Resolve protocol preference
    let config = resolve_protocol_config(&state.pool, pattern.media_type_id).await?;
    let preferred = config.preferred()?;

    // Use shared pattern match config resolution
    let match_config = crate::watch::resolve_pattern_match_config(pattern, None, &[]);

    // Resolve allowed categories for post-filtering
    let allowed_categories = if let Some(mt_id) = pattern.media_type_id {
        resolve_categories_for_media_type(&state.pool, Some(mt_id))
            .await
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    // Resolve quality profile for scoring candidates
    let quality_profile = if let Some(mt_id) = pattern.media_type_id {
        let mut cache = std::collections::HashMap::new();
        crate::watch::resolve_quality_profile(&state.pool, mt_id, &mut cache).await
    } else {
        None
    };

    let trash_filters = state.trash_filters.get_filters();

    // Convert search results to Release and score through the unified pipeline
    let releases: Vec<Release> = search_result
        .results
        .iter()
        .map(Release::from_search_result)
        .collect();

    // Pre-compute episodes_needed for TvSeason patterns, mirroring the RSS path.
    let episodes_needed = pattern
        .media_want
        .as_ref()
        .and_then(episode_key::get_episodes_needed);

    let mut config = crate::services::want::scoring::ScoringConfig::new(
        &match_config.match_targets,
        &match_config.patterns,
        match_config.min_score,
        quality_profile.as_ref(),
        trash_filters.as_deref(),
    )
    .with_allowed_categories(&allowed_categories)
    .with_preferred_protocol(preferred)
    .with_match_mode(&match_config.match_mode);

    // Structural gating. Without these the final search bypasses every
    // structural reject (pack preference and recency, unaired episodes, season
    // mismatch, publish-date sanity) — `structural_reject` returns early when
    // `media_want` is None, so a season pack could be grabbed to satisfy a hold
    // on a single episode of a still-airing season.
    if let Some(ref mw) = parsed_media_want {
        config = config.with_media_want(mw);
    }
    if let Some(ref eps) = episodes_needed
        && let Some(ref mw_json) = pattern.media_want
    {
        config = config.with_episodes_needed(eps, mw_json);
    }

    let scored = crate::services::want::scoring::score_batch(&releases, &config);

    // Select best candidate: prefer preferred protocol, fall back to highest score
    let mut best_preferred: Option<&crate::watch::ProcessedRelease> = None;
    let mut best_overall: Option<&crate::watch::ProcessedRelease> = None;

    for pr in &scored {
        if !pr.is_candidate() {
            continue;
        }
        // Skip releases without a download URL
        if pr.release.download_url.is_none() && pr.release.magnet_url.is_none() {
            continue;
        }
        // The final search replaces one specific held release, so a candidate
        // must cover the held episode and nothing wider. Without this a hold on
        // a single episode can be "upgraded" into a whole season pack.
        if !replacement_covers_held_episode(held_match, &pattern.media_want, pr) {
            continue;
        }

        if pr.is_preferred_protocol
            && best_preferred
                .as_ref()
                .is_none_or(|b| pr.total_score > b.total_score)
        {
            best_preferred = Some(pr);
            continue;
        }
        if best_overall
            .as_ref()
            .is_none_or(|b| pr.total_score > b.total_score)
        {
            best_overall = Some(pr);
        }
    }

    // Prefer preferred-protocol result
    if let Some(pr) = best_preferred {
        info!(
            match_id = %held_match.id,
            title = %pr.release.title,
            protocol = %pr.release.protocol,
            "Final search found preferred-protocol release"
        );
        return Some(pr.clone());
    }

    // Fall back to a better-scored non-preferred result
    if let Some(pr) = best_overall
        && pr.total_score > held_match.match_score
    {
        info!(
            match_id = %held_match.id,
            title = %pr.release.title,
            new_score = pr.total_score,
            held_score = held_match.match_score,
            "Final search found better-scored release"
        );
        return Some(pr.clone());
    }

    None
}

/// Create a download for a matched release (also used by service.rs handle_match)
pub async fn create_download_for_match(
    state: &AppState,
    pattern: &watch_repo::WatchPatternRow,
    release: &Release,
) -> Option<Uuid> {
    let download_url = release
        .download_url
        .clone()
        .or_else(|| release.magnet_url.clone())?;
    let protocol = release.protocol.clone();

    let metadata = serde_json::json!({
        "watch_pattern_id": pattern.id,
        "source_system": pattern.source_system,
        "source_id": pattern.source_id,
        "original_metadata": pattern.metadata,
    });

    let media_kind = resolve_media_kind(&state.pool, pattern).await;

    let submission = DownloadSubmission {
        source: "watch".to_string(),
        protocol,
        name: release.title.clone(),
        download_url,
        download_client_id: None,
        callback_url: Some(pattern.callback_url.clone()),
        auth_token: None,
        metadata,
        release_payload: Some(release.raw_data.clone()),
        api_key_id: None,
        url_context: DownloadUrlContext::default(),
        indexer_source_id: release.indexer_source_id,
        quality_profile_id: None,
        media_kind,
        skip_duplicate_url: true,
    };

    let ctx = DownloadServiceContext {
        pool: &state.pool,
        http_client: &state.http_client,
        download_events: &state.download_events,
        download_clients: state.download_clients.as_ref(),
        indexers: state.indexers.as_ref(),
    };

    match submit_download(&ctx, submission).await {
        Ok(outcome) => {
            info!(download_id = %outcome.id, release = %release.title, "Created download from watch match");
            Some(outcome.id)
        }
        Err(e) => {
            error!(error = %e, release = %release.title, "Failed to create download from watch match");
            None
        }
    }
}

/// Resolve a watch pattern's `media_type_id` to its key (e.g. "tv", "movie").
async fn resolve_media_kind(
    pool: &sqlx::PgPool,
    pattern: &watch_repo::WatchPatternRow,
) -> Option<String> {
    // Prefer the want's own kind: sport wants are filed under the "tv" media
    // type (there is no "sports" media_type bucket), so the media_type key would
    // yield "tv" and parse_release would drop the sport's location/round/session
    // before the interpretation reaches kobbler. parse_media_kind maps a
    // sport_event/sport_season want to "sports". Fall back to the media_type key
    // for patterns with no structured want.
    if let Some(mw) = pattern
        .media_want
        .as_ref()
        .and_then(|v| serde_json::from_value::<nats_jobs::MediaWant>(v.clone()).ok())
    {
        return Some(crate::media::parse_media_kind(&mw).to_string());
    }

    let id = pattern.media_type_id?;
    media_types::get_by_id(pool, id)
        .await
        .ok()?
        .map(|mt| mt.key)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn media_want_season() -> serde_json::Value {
        serde_json::json!({
            "kind": "tv_season",
            "season": "2",
            "show_name": "Dark Matter",
            "year": 2024,
        })
    }

    fn held_match_with_key(episode_key: Option<&str>) -> watch_repo::WatchMatchRow {
        watch_repo::WatchMatchRow {
            id: Uuid::new_v4(),
            watch_pattern_id: Uuid::new_v4(),
            indexer_source_id: None,
            release_guid: "guid".to_string(),
            release_title: "Dark Matter S02E02 720p WEB-DL x265".to_string(),
            release_data: None,
            match_score: 0.8,
            matched_pattern: String::new(),
            action: "held".to_string(),
            download_id: None,
            callback_sent_at: None,
            callback_error: None,
            created_at: time::OffsetDateTime::now_utc(),
            hold_until: None,
            held: true,
            callback_attempts: 0,
            callback_last_attempt_at: None,
            quality_score: None,
            rejected: false,
            rejected_at: None,
            rejected_reason: None,
            import_confirmed_at: None,
            episode_key: episode_key.map(String::from),
        }
    }

    fn candidate(title: &str) -> crate::watch::ProcessedRelease {
        crate::watch::ProcessedRelease {
            release: Release {
                guid: Uuid::new_v4().to_string(),
                title: title.to_string(),
                file_name: None,
                size: 1_000_000_000,
                indexer: "test".to_string(),
                protocol: "torrent".to_string(),
                download_url: Some("http://example.com/dl".to_string()),
                magnet_url: None,
                info_url: None,
                publish_date: "2026-09-03T00:00:00Z".to_string(),
                category_ids: Vec::new(),
                info_hash: None,
                files: None,
                seeders: Some(100),
                peers: None,
                leechers: None,
                grabs: None,
                votes: None,
                indexer_id: None,
                indexer_source_id: None,
                raw_data: serde_json::json!({}),
                external_ids: Vec::new(),
                media: None,
            },
            decision: nats_jobs::ScoringDecision::Candidate,
            reject_reasons: Vec::new(),
            match_score: Some(0.9),
            matched_pattern: None,
            matched_target: None,
            parsed_quality: None,
            quality_score: Some(0.5),
            quality_rejected: false,
            quality_reject_reasons: Vec::new(),
            structural_rejected: false,
            structural_reject_reason: None,
            total_score: 1.4,
            episode_key: None,
            is_preferred_protocol: true,
            structural_bonus: 0.0,
            seeder_bonus: 0.0,
            freshness_bonus: 0.0,
            context_flags: Vec::new(),
        }
    }

    /// The regression: a hold on a single episode must not be "upgraded" into a
    /// full season pack, which for a still-airing season drags in unaired episodes.
    #[test]
    fn season_pack_rejected_as_replacement_for_single_episode_hold() {
        let held = held_match_with_key(Some("S02E02"));
        let mw = Some(media_want_season());
        let pack = candidate("Dark.Matter.S02.REPACK.1080p.WEB-DL.DD5.1.H.264-VietHD");

        assert!(
            !replacement_covers_held_episode(&held, &mw, &pack),
            "a season pack must not replace a single-episode hold"
        );
    }

    #[test]
    fn same_episode_accepted_as_replacement() {
        let held = held_match_with_key(Some("S02E02"));
        let mw = Some(media_want_season());
        let better = candidate("Dark.Matter.2024.S02E02.1080p.WEB-DL.DDP5.1.H.264-FLUX");

        assert!(
            replacement_covers_held_episode(&held, &mw, &better),
            "a same-episode release must be an acceptable replacement"
        );
    }

    #[test]
    fn different_episode_rejected_as_replacement() {
        let held = held_match_with_key(Some("S02E02"));
        let mw = Some(media_want_season());
        let other = candidate("Dark.Matter.2024.S02E05.1080p.WEB-DL.DDP5.1.H.264-FLUX");

        assert!(
            !replacement_covers_held_episode(&held, &mw, &other),
            "a different episode must not replace the held one"
        );
    }

    #[test]
    fn pack_may_replace_a_pack() {
        let held = held_match_with_key(Some(episode_key::SEASON_PACK_KEY));
        let mw = Some(media_want_season());
        let pack = candidate("Dark.Matter.S02.REPACK.1080p.WEB-DL.DD5.1.H.264-VietHD");

        assert!(
            replacement_covers_held_episode(&held, &mw, &pack),
            "a pack hold may be replaced by a better pack"
        );
    }

    #[test]
    fn no_episode_key_is_unconstrained() {
        let held = held_match_with_key(None);
        let mw = Some(media_want_season());
        let anything = candidate("Some.Movie.2024.1080p.WEB-DL.H.264-GROUP");

        assert!(
            replacement_covers_held_episode(&held, &mw, &anything),
            "patterns without an episode key must stay unconstrained"
        );
    }
}
