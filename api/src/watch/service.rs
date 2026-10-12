use sqlx::PgPool;
use std::collections::{HashMap, HashSet};
use std::time::Duration;
use tokio::time::sleep;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use super::holds::{
    check_and_supersede_holds, create_download_for_match, process_expired_holds,
    should_hold_for_protocol,
};
use super::matcher::match_patterns;
use super::types::{Release, WatchMatchResponse};
use crate::cpu_bound::run_cpu_bound;
use crate::decision_log::{self, DecisionCollector, LogProviderCall, LogRelease};
use crate::helpers::{release_matches_categories, resolve_categories_for_source};
use crate::media::{
    match_targets_from_media_want, media_type_key_to_quality_kind, search_terms_from_media_want,
    xem_match_targets, xem_season_match_targets,
};
use crate::nats;
use crate::quality::score_quality;
use crate::repositories::app_settings;
use crate::repositories::media_types;
use crate::repositories::quality_profiles::{self, QualityProfileRow};
use crate::repositories::watch as watch_repo;
use crate::scene_mappings::SceneTitle;
use crate::services::upgrade;
use crate::state::AppState;
use crate::trash_filters::TrashFilterSet;
use crate::types::WatchStreamEvent;
use crate::watch::score_against_targets;
use crate::xem::XemShowMap;

/// Check if a release is excluded by the pattern's excluded_releases list.
/// Returns true if the release should be skipped.
pub fn is_release_excluded(pattern: &watch_repo::WatchPatternRow, release_title: &str) -> bool {
    let excluded = match &pattern.excluded_releases {
        Some(val) => val,
        None => return false,
    };
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

/// Check if there are any active watch patterns
pub async fn has_active_patterns(pool: &PgPool) -> bool {
    watch_repo::has_active_patterns(pool).await.unwrap_or(false)
}

/// Current download state for a watch pattern.
pub struct PatternDownloadState {
    /// Has a non-failed, non-cancelled download that hasn't been imported yet.
    pub has_active: bool,
    /// Has at least one download that was successfully imported.
    pub has_imported: bool,
}

/// Query both active and imported download state for a pattern in one call.
pub async fn get_pattern_download_state(pool: &PgPool, pattern_id: Uuid) -> PatternDownloadState {
    let has_active = watch_repo::pattern_has_active_download(pool, pattern_id)
        .await
        .unwrap_or(false);
    let has_imported = watch_repo::pattern_has_imported_download(pool, pattern_id)
        .await
        .unwrap_or(false);
    PatternDownloadState {
        has_active,
        has_imported,
    }
}

/// Get all enabled indexer sources that are due for a check
pub async fn get_sources_to_check(pool: &PgPool) -> Vec<watch_repo::IndexerSourceRow> {
    watch_repo::get_sources_to_check(pool)
        .await
        .unwrap_or_else(|e| {
            error!(error = %e, "failed to fetch indexer sources");
            Vec::new()
        })
}

/// Get all enabled watch patterns that could match releases from a given indexer source
pub async fn get_matching_patterns(
    pool: &PgPool,
    source: &watch_repo::IndexerSourceRow,
) -> Vec<watch_repo::WatchPatternRow> {
    // If the source serves specific media types, get patterns matching those types.
    // If the source has no media_type_ids (serves all), get all patterns.
    if source.media_type_ids.is_empty() {
        return watch_repo::get_all_enabled_patterns(pool)
            .await
            .unwrap_or_else(|e| {
                error!(error = %e, "failed to fetch all watch patterns");
                Vec::new()
            });
    }

    // Fetch patterns for ALL media types this source serves, deduplicating by ID
    let mut all_patterns = Vec::new();
    let mut seen_ids = HashSet::new();

    for mt_id in &source.media_type_ids {
        match watch_repo::get_patterns_for_media_type(pool, *mt_id).await {
            Ok(patterns) => {
                for p in patterns {
                    if seen_ids.insert(p.id) {
                        all_patterns.push(p);
                    }
                }
            }
            Err(e) => {
                error!(error = %e, media_type_id = %mt_id, "failed to fetch matching watch patterns");
            }
        }
    }

    all_patterns
}

/// Check if a release has already been matched for a pattern
pub async fn already_matched(pool: &PgPool, pattern_id: Uuid, release_guid: &str) -> bool {
    watch_repo::already_matched(pool, pattern_id, release_guid)
        .await
        .unwrap_or(false)
}

/// Record a match and take action.
///
/// If the profile has `protocol_priority` configured and the release's protocol
/// is not the top priority, the match is held for `protocol_delay_minutes` to
/// allow a higher-priority protocol match to appear.
///
/// Uses atomic insert to prevent race conditions where two threads process
/// the same release concurrently and both create downloads.
///
/// Acquires `pattern_search_locks` for the pattern. If the lock is already
/// held by another session, returns `None` (the other session will commit
/// its own download). Callers that already hold the lock — backlog handler,
/// search-now, research re-search — must call `handle_match_locked` instead;
/// re-acquiring would deadlock since `tokio::sync::Mutex` is non-reentrant.
#[allow(clippy::too_many_arguments)]
pub async fn handle_match(
    state: &AppState,
    pattern: &watch_repo::WatchPatternRow,
    release: &Release,
    match_score: f64,
    matched_pattern: &str,
    source_id: Option<Uuid>,
    quality_score: Option<f64>,
    is_upgrade: bool,
    replaces_download_id: Option<Uuid>,
    episode_key: Option<&str>,
) -> Option<Uuid> {
    let _lock = state.pattern_search_locks.try_acquire(pattern.id)?;
    handle_match_locked(
        state,
        pattern,
        release,
        match_score,
        matched_pattern,
        source_id,
        quality_score,
        is_upgrade,
        replaces_download_id,
        episode_key,
    )
    .await
}

/// Body of `handle_match` that assumes the caller already holds the
/// per-pattern search lock. Use this from the backlog/want/manual/research
/// paths where the lock is acquired around the search-and-dispatch session.
#[allow(clippy::too_many_arguments)]
pub async fn handle_match_locked(
    state: &AppState,
    pattern: &watch_repo::WatchPatternRow,
    release: &Release,
    match_score: f64,
    matched_pattern: &str,
    source_id: Option<Uuid>,
    quality_score: Option<f64>,
    is_upgrade: bool,
    replaces_download_id: Option<Uuid>,
    episode_key: Option<&str>,
) -> Option<Uuid> {
    // Structural validation: parse release and check against MediaWant
    if let Some(media_want_json) = pattern.media_want.as_ref()
        && let Ok(media_want) =
            serde_json::from_value::<nats_jobs::MediaWant>(media_want_json.clone())
        && let Some(reason) = crate::services::want::structural_reject_from_want(
            release,
            &media_want,
            crate::services::want::SearchContext::Rss,
        )
    {
        info!(
            pattern_id = %pattern.id,
            release = %release.title,
            reason = %reason,
            "Structural rejection in handle_match"
        );
        return None;
    }

    // Check if this release should be held due to protocol priority
    if pattern.auto_download {
        if let Some(hold_until) = should_hold_for_protocol(state, pattern, release).await {
            // Check if any existing held matches for this pattern should be superseded
            let _ = check_and_supersede_holds(state, pattern, release).await;

            let match_input = watch_repo::WatchMatchInput {
                pattern_id: pattern.id,
                indexer_source_id: source_id,
                release_guid: &release.guid,
                release_title: &release.title,
                release_data: Some(&release.raw_data),
                match_score,
                matched_pattern,
                action: "held",
                download_id: None,
                hold_until: Some(hold_until),
                held: true,
                quality_score,
                rejected: false,
                rejected_at: None,
                rejected_reason: None,
                episode_key,
            };

            // Use atomic insert - if another thread already created this match, skip
            let result = watch_repo::create_match_if_not_exists(&state.pool, &match_input).await;
            match result {
                Ok(Some(_match_id)) => {
                    info!(
                        pattern_id = %pattern.id,
                        release = %release.title,
                        protocol = %release.protocol,
                        hold_until = %hold_until,
                        "Holding match for protocol delay"
                    );
                }
                Ok(None) => {
                    debug!(
                        pattern_id = %pattern.id,
                        release = %release.title,
                        "Race avoided: another thread already matched this release (hold)"
                    );
                }
                Err(e) => {
                    error!(error = %e, "Failed to create watch match (hold)");
                }
            }
            return None;
        }

        // This is a top-priority protocol match — supersede any held matches
        let _ = check_and_supersede_holds(state, pattern, release).await;
    }

    let action = if is_upgrade {
        "upgrade"
    } else if pattern.auto_download {
        "downloaded"
    } else {
        "notified"
    };

    // Augment release_data with upgrade metadata for callback retry
    let release_data = if let (true, Some(replaces_id)) = (is_upgrade, replaces_download_id) {
        let mut data = release.raw_data.clone();
        if let Some(obj) = data.as_object_mut() {
            obj.insert(
                "replaces_download_id".to_string(),
                serde_json::json!(replaces_id.to_string()),
            );
        }
        data
    } else {
        release.raw_data.clone()
    };

    // Defence-in-depth duplicate check, re-read inside the lock.
    //
    // The caller's pre-search gate ran before a multi-second indexer round
    // trip, so it reflects stale state: two failure-driven re-searches for the
    // same pattern can both pass it and then both land here with *different*
    // releases of the same title. The atomic match insert below does not catch
    // that — it is keyed on (pattern, release_guid), which differs.
    //
    // Upgrades are exempt: replacing an imported copy is the whole point, and
    // `replaces_download_id` tracks what is being superseded. Seasons are
    // exempt too — they track state per episode, not per pattern.
    if !is_upgrade
        && episode_key.is_none()
        && watch_repo::pattern_has_active_download(&state.pool, pattern.id)
            .await
            .unwrap_or(false)
    {
        info!(
            pattern_id = %pattern.id,
            release = %release.title,
            "Skipping match: pattern already has an active download"
        );
        return None;
    }

    // Create match record first with atomic insert to win the race
    // If we don't win, another thread already handled this release
    let match_input = watch_repo::WatchMatchInput {
        pattern_id: pattern.id,
        indexer_source_id: source_id,
        release_guid: &release.guid,
        release_title: &release.title,
        release_data: Some(&release_data),
        match_score,
        matched_pattern,
        action,
        download_id: None, // Will update after download creation
        hold_until: None,
        held: false,
        quality_score,
        rejected: false,
        rejected_at: None,
        rejected_reason: None,
        episode_key,
    };

    let match_id = match watch_repo::create_match_if_not_exists(&state.pool, &match_input).await {
        Ok(Some(id)) => id,
        Ok(None) => {
            // Another thread already matched this release - skip to avoid duplicate download
            debug!(
                pattern_id = %pattern.id,
                release = %release.title,
                "Race avoided: another thread already matched this release"
            );
            return None;
        }
        Err(e) => {
            error!(error = %e, "Failed to create watch match");
            return None;
        }
    };

    // Now safe to create download - we won the race
    let download_id = if pattern.auto_download || is_upgrade {
        let dl_id = create_download_for_match(state, pattern, release).await;
        if let Some(id) = dl_id {
            let _ = watch_repo::update_match_download_id(&state.pool, match_id, id).await;
            let cancelled = watch_repo::cancel_held_matches_for_pattern(
                &state.pool,
                pattern.id,
                episode_key,
                match_id,
            )
            .await
            .unwrap_or(0);
            if cancelled > 0 {
                info!(
                    pattern_id = %pattern.id,
                    cancelled,
                    "Cancelled held matches after download"
                );
            }
        } else {
            // create_download failed — don't leave the reserved row as orphan downloaded.
            let _ =
                watch_repo::update_match_skipped(&state.pool, match_id, "create_download failed")
                    .await;
        }
        dl_id
    } else {
        None
    };

    // Broadcast watch match event for live UI updates
    let _ = state.watch_events.send(WatchStreamEvent::Created {
        watch_match: Box::new(WatchMatchResponse {
            id: match_id,
            watch_pattern_id: Some(pattern.id),
            indexer_source_id: source_id,
            release_guid: release.guid.clone(),
            release_title: release.title.clone(),
            release_data: release.raw_data.clone(),
            match_score,
            matched_pattern: Some(matched_pattern.to_string()),
            action: action.to_string(),
            download_id,
            callback_sent_at: None,
            callback_error: None,
            quality_score,
            rejected: false,
            rejected_at: None,
            rejected_reason: None,
            held: false,
            hold_until: None,
            created_at: time::OffsetDateTime::now_utc(),
            import_confirmed_at: None,
            episode_key: episode_key.map(String::from),
        }),
    });

    let watch_event = nats_jobs::WatchMatchedEvent {
        watch_pattern_id: pattern.id,
        source_system: pattern.source_system.clone(),
        source_id: pattern.source_id.clone(),
        match_score,
        matched_pattern: matched_pattern.to_string(),
        release_title: release.title.clone(),
        release_guid: release.guid.clone(),
        download_id,
        action: action.to_string(),
        metadata: pattern.metadata.clone(),
        is_upgrade,
        quality_score,
        replaces_download_id,
        episode_key: episode_key.map(String::from),
    };

    send_watch_callback(state, pattern, &watch_event, match_id).await;

    download_id
}

/// Send callback notification to consumer via NATS.
///
/// Watch match callbacks are delivered exclusively via NATS. The HTTP callback_url
/// points to kobbler's ingest endpoint which expects a different payload shape
/// (KobloadCallbackRequest), so sending WatchMatchedEvent over HTTP always fails (422).
async fn send_watch_callback(
    state: &AppState,
    pattern: &watch_repo::WatchPatternRow,
    event: &nats_jobs::WatchMatchedEvent,
    match_id: Uuid,
) {
    if state.nats.is_available() {
        nats::publish_watch_matched(&state.nats, event).await;

        let _ = watch_repo::mark_match_callback_sent(&state.pool, match_id).await;
        info!(pattern_id = %pattern.id, "Watch match callback sent via NATS");
    } else {
        // NATS unavailable — mark as sent to prevent infinite retries.
        // The match is recorded in the DB; consumer will see it on next poll.
        let _ = watch_repo::mark_match_callback_sent(&state.pool, match_id).await;
        warn!(
            pattern_id = %pattern.id,
            "NATS unavailable, watch match callback skipped (marked as sent)"
        );
    }
}

/// Update indexer source state after checking
pub async fn update_source_state(
    pool: &PgPool,
    source_id: Uuid,
    state: Option<serde_json::Value>,
    error: Option<String>,
) {
    let _ =
        watch_repo::update_indexer_source_state(pool, source_id, state.as_ref(), error.as_deref())
            .await;
}

/// Resolve the default quality profile for a media type, with caching.
pub async fn resolve_quality_profile(
    pool: &PgPool,
    media_type_id: Uuid,
    cache: &mut HashMap<Uuid, Option<QualityProfileRow>>,
) -> Option<QualityProfileRow> {
    if let Some(cached) = cache.get(&media_type_id) {
        return cached.clone();
    }

    let profile = async {
        let media_type = media_types::get_by_id(pool, media_type_id).await.ok()??;
        let quality_kind = media_type_key_to_quality_kind(&media_type.key);
        quality_profiles::get_default_for_media_kind(pool, quality_kind)
            .await
            .ok()?
    }
    .await;

    cache.insert(media_type_id, profile.clone());
    profile
}

/// Resolve an XEM show map for a pattern's media_want (if it's a TvEpisode with a TVDB ID).
pub async fn resolve_xem_for_pattern(
    state: &AppState,
    pattern: &watch_repo::WatchPatternRow,
) -> Option<XemShowMap> {
    let media_want = pattern
        .media_want
        .as_ref()
        .and_then(|v| serde_json::from_value::<nats_jobs::MediaWant>(v.clone()).ok())?;

    let tvdb_id = match &media_want {
        nats_jobs::MediaWant::TvEpisode { identifiers, .. }
        | nats_jobs::MediaWant::TvSeason { identifiers, .. } => identifiers
            .as_ref()
            .and_then(|i| i.tvdb_id.as_ref()?.parse::<i64>().ok())?,
        _ => return None,
    };

    state
        .xem_cache
        .get_show_map(&state.http_client, &state.pool, tvdb_id)
        .await
}

/// Resolve alternate show titles for a pattern's media_want (if it's TV with a
/// TVDB ID).
pub async fn resolve_scene_titles_for_pattern(
    state: &AppState,
    pattern: &watch_repo::WatchPatternRow,
) -> Vec<SceneTitle> {
    let Some(media_want) = pattern
        .media_want
        .as_ref()
        .and_then(|v| serde_json::from_value::<nats_jobs::MediaWant>(v.clone()).ok())
    else {
        return Vec::new();
    };

    crate::scene_titles::resolve(
        &state.scene_mappings,
        &state.xem_cache,
        &state.http_client,
        &state.pool,
        &media_want,
    )
    .await
}

/// Grouping key for batch selection: (pattern_id, episode_key).
type CandidateKey = (Uuid, Option<String>);

/// Candidate entry: ProcessedRelease + metadata for selection/execution.
struct CandidateEntry {
    processed: crate::watch::ProcessedRelease,
    source_id: Uuid,
    _pattern_id: Uuid,
    is_upgrade: bool,
    replaces_download_id: Option<Uuid>,
}

/// Pre-computed pattern data for the scoring hot loop.
///
/// All async/DB work is done once per pattern during preparation.
/// The inner release loop uses only in-memory lookups.
struct PreparedPattern {
    pattern: watch_repo::WatchPatternRow,
    quality_profile: Option<QualityProfileRow>,
    match_config: ResolvedMatchConfig,
    categories: Vec<i32>,
    matched_guids: HashSet<String>,
    _is_season: bool,
    /// Pre-parsed MediaWant (avoids repeated serde_json::from_value per release).
    parsed_media_want: Option<nats_jobs::MediaWant>,
    /// Pre-computed episodes_needed for TvSeason patterns.
    episodes_needed: Option<Vec<nats_jobs::EpisodeRef>>,
    /// Whether this pattern's preferred protocol is set and what it is.
    preferred_protocol: Option<String>,
}

/// Prepare all pattern-invariant data for a set of patterns.
///
/// Performs all DB and async lookups once per pattern:
/// - Quality profile resolution (cached across patterns sharing media_type_id)
/// - XEM show map resolution
/// - Match config (targets, patterns, mode, min_score)
/// - Category resolution for the source
/// - Batch-loaded matched GUIDs
/// - Protocol preference
async fn prepare_patterns(
    state: &AppState,
    source: &watch_repo::IndexerSourceRow,
    patterns: &[watch_repo::WatchPatternRow],
    profile_cache: &mut HashMap<Uuid, Option<QualityProfileRow>>,
    protocol_cache: &mut HashMap<Option<Uuid>, Option<String>>,
) -> Vec<PreparedPattern> {
    let mut prepared = Vec::with_capacity(patterns.len());

    // Cache categories per media_type_id (same source + media_type = same categories)
    let mut category_cache: HashMap<Option<Uuid>, Vec<i32>> = HashMap::new();

    for pattern in patterns {
        // Resolve quality profile (cached across patterns)
        let quality_profile = if let Some(mt_id) = pattern.media_type_id {
            resolve_quality_profile(&state.pool, mt_id, profile_cache).await
        } else {
            None
        };

        // Resolve XEM show map
        let xem_show_map = resolve_xem_for_pattern(state, pattern).await;

        // Resolve Sonarr scene mapping titles
        let scene_titles = resolve_scene_titles_for_pattern(state, pattern).await;

        // Resolve match config (targets, patterns, mode, min_score + XEM/scene augmentation)
        let match_config =
            resolve_pattern_match_config(pattern, xem_show_map.as_ref(), &scene_titles);

        // Resolve categories (cached per media_type_id for this source)
        let categories = if let Some(mt_id) = pattern.media_type_id {
            if let Some(cached) = category_cache.get(&Some(mt_id)) {
                cached.clone()
            } else {
                let cats = resolve_categories_for_source(&state.pool, source, Some(mt_id))
                    .await
                    .unwrap_or_default();
                category_cache.insert(Some(mt_id), cats.clone());
                cats
            }
        } else {
            Vec::new()
        };

        // Batch-load matched GUIDs
        let matched_guids = watch_repo::matched_guids_for_pattern(&state.pool, pattern.id)
            .await
            .unwrap_or_default();

        let is_season = pattern
            .media_want
            .as_ref()
            .is_some_and(super::episode_key::is_tv_season);

        // Pre-parse MediaWant
        let parsed_media_want = pattern
            .media_want
            .as_ref()
            .and_then(|v| serde_json::from_value::<nats_jobs::MediaWant>(v.clone()).ok());

        // Pre-compute episodes_needed for TvSeason
        let episodes_needed = if is_season {
            pattern
                .media_want
                .as_ref()
                .and_then(super::episode_key::get_episodes_needed)
        } else {
            None
        };

        // Resolve preferred protocol (cached across patterns)
        let preferred_protocol = {
            let entry = protocol_cache
                .entry(pattern.media_type_id)
                .or_insert_with(|| None);
            if entry.is_none()
                && let Some(config) = crate::services::protocol::resolve_protocol_config(
                    &state.pool,
                    pattern.media_type_id,
                )
                .await
            {
                *entry = config.preferred().map(|s| s.to_string());
            }
            entry.clone()
        };

        prepared.push(PreparedPattern {
            pattern: pattern.clone(),
            quality_profile,
            match_config,
            categories,
            matched_guids,
            _is_season: is_season,
            parsed_media_want,
            episodes_needed,
            preferred_protocol,
        });
    }

    prepared
}

/// Build a `ScoringConfig` from a `PreparedPattern` for the unified scoring pipeline.
fn build_scoring_config<'a>(
    prep: &'a PreparedPattern,
    trash_filters: Option<&'a TrashFilterSet>,
) -> crate::services::want::scoring::ScoringConfig<'a> {
    let mut config = crate::services::want::scoring::ScoringConfig::new(
        &prep.match_config.match_targets,
        &prep.match_config.patterns,
        prep.match_config.min_score,
        prep.quality_profile.as_ref(),
        trash_filters,
    )
    .with_match_mode(&prep.match_config.match_mode)
    .with_search_context(crate::services::want::scoring::SearchContext::Rss);

    if let Some(ref proto) = prep.preferred_protocol {
        config = config.with_preferred_protocol(proto);
    }

    if let Some(ref mw) = prep.parsed_media_want {
        config = config.with_media_want(mw);
    }

    if let Some(ref eps) = prep.episodes_needed
        && let Some(ref mw_json) = prep.pattern.media_want
    {
        config = config.with_episodes_needed(eps, mw_json);
    }

    config
}

/// Record a release evaluation in the decision collector.
fn record_scoring_result(
    collector: &DecisionCollector,
    provider_call_id: Uuid,
    pr: &crate::watch::ProcessedRelease,
    prep: &PreparedPattern,
) {
    use nats_jobs::ScoringDecision;

    // Skip no-match and excluded noise from the decision log
    match pr.decision {
        ScoringDecision::NoMatch | ScoringDecision::Excluded => return,
        _ => {}
    }

    let quality_breakdown = if pr.quality_rejected || pr.quality_score.is_some() {
        // Re-score to get the breakdown (ProcessedRelease doesn't carry it)
        prep.quality_profile
            .as_ref()
            .map(|p| score_quality(&pr.release.title, Some(p), None, None))
            .map(|q| q.breakdown)
    } else {
        None
    };

    let structural_result = if pr.structural_rejected {
        Some(decision_log::StructuralResult {
            checked: true,
            matched: false,
            reject_reason: pr.structural_reject_reason.as_ref().map(|r| r.to_string()),
            ..Default::default()
        })
    } else {
        None
    };

    let decision_reason = pr
        .reject_reasons
        .first()
        .map(|r| r.to_string())
        .or_else(|| pr.quality_reject_reasons.first().map(|r| r.to_string()));

    collector.record_release(LogRelease {
        provider_call_id: Some(provider_call_id),
        release_title: pr.release.title.clone(),
        release_guid: Some(pr.release.guid.clone()),
        release_size: Some(pr.release.size),
        protocol: Some(pr.release.protocol.clone()),
        indexer_name: None,
        match_score: pr.match_score,
        matched_pattern: pr.matched_pattern.clone(),
        match_mode: Some(prep.match_config.match_mode.clone()),
        quality_score: pr.quality_score,
        quality_score_breakdown: quality_breakdown,
        total_score: Some(pr.total_score),
        parsed_quality: pr
            .parsed_quality
            .as_ref()
            .and_then(|pq| serde_json::to_value(pq).ok()),
        structural_result,
        decision: pr.decision.to_string(),
        decision_reason,
        episode_key: pr.episode_key.clone(),
        is_best: false,
        release_data: None,
    });
}

/// Score all releases from a source against all matching patterns.
///
/// Uses a pattern-outer loop: all pattern-invariant data (quality profile,
/// XEM, categories, matched GUIDs) is pre-computed once per pattern. The
/// inner release loop uses only in-memory checks — zero DB queries.
///
/// Only retains the single best candidate per (pattern_id, episode_key) group.
#[allow(clippy::too_many_arguments)]
async fn score_source_releases(
    state: &AppState,
    source: &watch_repo::IndexerSourceRow,
    releases: &[Release],
    patterns: &[watch_repo::WatchPatternRow],
    candidates: &mut HashMap<CandidateKey, CandidateEntry>,
    profile_cache: &mut HashMap<Uuid, Option<QualityProfileRow>>,
    protocol_cache: &mut HashMap<Option<Uuid>, Option<String>>,
    collector: Option<(&DecisionCollector, Uuid)>,
    mut review_queue: Option<&mut HashMap<Uuid, Vec<crate::watch::ProcessedRelease>>>,
) {
    use crate::services::want::koblem_review;
    use crate::services::want::scoring::{release_matches, score_release};

    // Phase 1: Pre-compute all pattern-invariant data (DB/async calls happen here)
    let prepared = prepare_patterns(state, source, patterns, profile_cache, protocol_cache).await;

    // Get trash filters once for all patterns
    let trash_filters = state.trash_filters.get_filters();

    // Phase 2: Score releases against prepared patterns (pure in-memory).
    // Regex-heavy quality parsing + Jaro-Winkler similarity over 200-300
    // releases × N patterns is pure CPU with no await points; run it via
    // run_cpu_bound so it can't starve JetStream consumers on the runtime.
    run_cpu_bound(|| {
        for prep in &prepared {
            // Skip patterns with no match config (no targets and no patterns)
            if prep.match_config.match_targets.is_empty() && prep.match_config.patterns.is_empty() {
                continue;
            }

            // Build ScoringConfig once per pattern
            let config = build_scoring_config(prep, trash_filters.as_deref());
            let review_title = review_queue
                .is_some()
                .then(|| koblem_review::review_wanted_title(prep.parsed_media_want.as_ref()));

            for release in releases {
                // In-memory dedup: skip if already matched in a previous cycle
                if prep.matched_guids.contains(&release.guid) {
                    continue;
                }

                // Check exclusion before scoring (info_hash matching not supported by ScoringConfig)
                if is_release_excluded(&prep.pattern, &release.title) {
                    continue;
                }

                // In-memory category validation (done before score_release since
                // ScoringConfig doesn't carry per-pattern categories for RSS)
                if prep.pattern.media_type_id.is_some()
                    && !prep.categories.is_empty()
                    && !release_matches_categories(&release.category_ids, &prep.categories)
                {
                    continue;
                }

                // Nearly every release × pattern pair is a no-match, and the full
                // gates (quality, TRaSH, structural parses) cost milliseconds each.
                // A no-match is only used by the decision log or by a koblem
                // review of a related title; otherwise it's dropped, so don't
                // compute it.
                if collector.is_none()
                    && !release_matches(release, &config)
                    && review_title
                        .as_deref()
                        .is_none_or(|t| !koblem_review::has_title_overlap(&release.title, t))
                {
                    continue;
                }

                let pr = score_release(release, &config);

                // Record evaluation in decision collector if enabled
                if let Some((coll, call_id)) = collector {
                    record_scoring_result(coll, call_id, &pr, prep);
                }

                // Collect processed release for koblem review, keyed by the pattern
                // that scored it. Dispatching per-pattern prevents cross-pattern
                // contamination (reject reasons computed against another pattern's
                // media_want leaking into this pattern's review request).
                if let Some(queue) = review_queue.as_deref_mut()
                    && let Some(title) = review_title.as_deref()
                    && koblem_review::worth_reviewing(&pr, title)
                {
                    queue.entry(prep.pattern.id).or_default().push(pr.clone());
                }

                if !pr.is_candidate() {
                    continue;
                }

                let key = (prep.pattern.id, pr.episode_key.clone());

                let entry = CandidateEntry {
                    processed: pr,
                    source_id: source.id,
                    _pattern_id: prep.pattern.id,
                    is_upgrade: false,
                    replaces_download_id: None,
                };

                // Keep only the best candidate per group
                match candidates.entry(key) {
                    std::collections::hash_map::Entry::Vacant(e) => {
                        e.insert(entry);
                    }
                    std::collections::hash_map::Entry::Occupied(mut e) => {
                        if entry.processed.is_better_than(&e.get().processed) {
                            e.insert(entry);
                        }
                    }
                }
            }
        }
    });
}

/// One pattern's slice of the pooled `processed_by_pattern` map, ready to hand
/// to `publish_review`. Pure-data — no IO, no async.
pub(crate) struct PatternDispatch<'a> {
    pub pattern: &'a watch_repo::WatchPatternRow,
    pub releases: &'a [crate::watch::ProcessedRelease],
}

/// Pair each pattern in `processed_by_pattern` with its `WatchPatternRow`.
///
/// Skips patterns missing from `all_patterns` (shouldn't happen in practice but
/// keeps the caller safe). Output is sorted by `pattern.id` so logs and tests
/// are deterministic.
pub(crate) fn koblem_dispatch_plan<'a>(
    processed_by_pattern: &'a HashMap<Uuid, Vec<crate::watch::ProcessedRelease>>,
    all_patterns: &'a HashMap<Uuid, watch_repo::WatchPatternRow>,
) -> Vec<PatternDispatch<'a>> {
    let mut plan: Vec<PatternDispatch<'a>> = processed_by_pattern
        .iter()
        .filter_map(|(pattern_id, releases)| {
            all_patterns.get(pattern_id).map(|pattern| PatternDispatch {
                pattern,
                releases: releases.as_slice(),
            })
        })
        .collect();
    plan.sort_by_key(|p| p.pattern.id);
    plan
}

/// Whether scored releases should be collected for koblem review at all.
async fn koblem_reviews_enabled(state: &AppState) -> bool {
    state.nats.is_available()
        && app_settings::get_koblem_settings(&state.pool)
            .await
            .unwrap_or_default()
            .enabled
}

/// Fire one koblem review per pattern that had at least one scored release.
///
/// This replaces the earlier per-winner dispatch inside `select_and_execute_winners`,
/// which leaked cross-pattern rejections into a single review request and
/// silently dropped patterns with zero kobload-accepted candidates. The
/// per-candidate filter (hard rejects, CAM/TS, zero title overlap) still lives
/// inside `publish_review`; this function just groups correctly.
async fn dispatch_koblem_reviews(
    state: &AppState,
    processed_by_pattern: &HashMap<Uuid, Vec<crate::watch::ProcessedRelease>>,
    all_patterns: &HashMap<Uuid, watch_repo::WatchPatternRow>,
    profile_cache: &mut HashMap<Uuid, Option<QualityProfileRow>>,
    priority: &str,
) {
    if processed_by_pattern.is_empty() || !koblem_reviews_enabled(state).await {
        return;
    }

    for entry in koblem_dispatch_plan(processed_by_pattern, all_patterns) {
        let pattern = entry.pattern;
        let media_want: Option<nats_jobs::MediaWant> = pattern
            .media_want
            .as_ref()
            .and_then(|v| serde_json::from_value(v.clone()).ok());
        let profile = if let Some(mt_id) = pattern.media_type_id {
            resolve_quality_profile(&state.pool, mt_id, profile_cache).await
        } else {
            None
        };
        crate::services::want::koblem_review::publish_review(
            &state.nats,
            entry.releases,
            media_want.as_ref(),
            profile.as_ref(),
            &pattern.source_id,
            priority,
        )
        .await;
    }
}

/// Execute downloads/holds for each pre-selected best candidate.
///
/// The `candidates` map already contains only the single best candidate per
/// (pattern_id, episode_key) group — selection happened during scoring via
/// `ProcessedRelease::is_better_than()`. This function only needs to run
/// upgrade checks and call `handle_match`.
async fn select_and_execute_winners(
    state: &AppState,
    candidates: HashMap<CandidateKey, CandidateEntry>,
    patterns: &HashMap<Uuid, watch_repo::WatchPatternRow>,
) {
    let mut profile_cache: HashMap<Uuid, Option<QualityProfileRow>> = HashMap::new();
    let trash_filters = state.trash_filters.get_filters();

    for ((pattern_id, episode_key), mut winner) in candidates {
        let pattern = match patterns.get(&pattern_id) {
            Some(p) => p,
            None => continue,
        };

        // Resolve quality profile for upgrade check
        let resolved_profile = if let Some(mt_id) = pattern.media_type_id {
            resolve_quality_profile(&state.pool, mt_id, &mut profile_cache).await
        } else {
            None
        };

        // Check download state and upgrade eligibility
        let is_season = episode_key.is_some()
            && pattern
                .media_want
                .as_ref()
                .is_some_and(super::episode_key::is_tv_season);

        if is_season {
            let ek = episode_key.as_deref().unwrap_or("");
            let episodes_needed = pattern
                .media_want
                .as_ref()
                .and_then(super::episode_key::get_episodes_needed);
            let library_quality = episodes_needed
                .as_ref()
                .and_then(|eps| super::episode_key::check_library_coverage(ek, eps));
            let uc = upgrade::check_episode_upgrade(
                &state.pool,
                pattern.id,
                ek,
                pattern.upgrade_allowed,
                false, // manual: RSS watch auto-path
                &winner.processed.release.title,
                Some(&winner.processed.release.publish_date),
                resolved_profile.as_ref(),
                trash_filters.as_deref(),
                library_quality,
            )
            .await;
            if uc.skip {
                debug!(
                    pattern_id = %pattern.id,
                    episode_key = ek,
                    reason = ?uc.skip_reason,
                    "Skipping batch group"
                );
                continue;
            }
            winner.is_upgrade = uc.is_upgrade;
            winner.replaces_download_id = uc.replaces_download_id;
        } else {
            let dl_state = get_pattern_download_state(&state.pool, pattern.id).await;
            let library_quality = pattern.media_want.as_ref().and_then(|mw| {
                let parsed: nats_jobs::MediaWant = serde_json::from_value(mw.clone()).ok()?;
                match parsed {
                    nats_jobs::MediaWant::Movie {
                        in_library: Some(in_lib),
                        library_video_height,
                        ..
                    }
                    | nats_jobs::MediaWant::SportEvent {
                        in_library: Some(in_lib),
                        library_video_height,
                        ..
                    } => Some((in_lib, library_video_height)),
                    _ => None,
                }
            });
            let uc = upgrade::check_pattern_upgrade(
                &state.pool,
                pattern.id,
                pattern.upgrade_allowed,
                false, // manual: RSS watch auto-path
                &dl_state,
                &winner.processed.release.title,
                Some(&winner.processed.release.publish_date),
                resolved_profile.as_ref(),
                trash_filters.as_deref(),
                library_quality,
            )
            .await;
            if uc.skip {
                debug!(
                    pattern_id = %pattern.id,
                    reason = ?uc.skip_reason,
                    "Skipping batch group"
                );
                continue;
            }
            winner.is_upgrade = uc.is_upgrade;
            winner.replaces_download_id = uc.replaces_download_id;
        }

        let pr = &winner.processed;
        info!(
            pattern_id = %pattern.id,
            release = %pr.release.title,
            protocol = %pr.release.protocol,
            match_score = ?pr.match_score,
            quality_score = ?pr.quality_score,
            total_score = pr.total_score,
            episode_key = ?episode_key,
            is_upgrade = winner.is_upgrade,
            "Batch selected best candidate"
        );

        handle_match(
            state,
            pattern,
            &pr.release,
            pr.match_score.unwrap_or(0.0),
            pr.matched_pattern.as_deref().unwrap_or(""),
            Some(winner.source_id),
            pr.quality_score,
            winner.is_upgrade,
            winner.replaces_download_id,
            episode_key.as_deref(),
        )
        .await;
    }
}

/// Process multiple releases against all matching patterns in a single batch.
///
/// All releases are scored together, keeping only the best candidate per
/// (pattern_id, episode_key) group. Then the single winner per group is
/// executed (download/hold). This is the primary API for RSS processing
/// where multiple releases arrive at once.
pub async fn process_releases(
    state: &AppState,
    source: &watch_repo::IndexerSourceRow,
    releases: &[Release],
    patterns: &[watch_repo::WatchPatternRow],
) {
    // Auto-index releases with info_hash into the hash index (background)
    auto_index_releases_to_hash_index(state.pool.clone(), releases);

    let mut candidates: HashMap<CandidateKey, CandidateEntry> = HashMap::new();
    let mut profile_cache: HashMap<Uuid, Option<QualityProfileRow>> = HashMap::new();
    let mut protocol_cache: HashMap<Option<Uuid>, Option<String>> = HashMap::new();
    let mut processed_by_pattern: HashMap<Uuid, Vec<crate::watch::ProcessedRelease>> =
        HashMap::new();
    let collect_reviews = koblem_reviews_enabled(state).await;
    score_source_releases(
        state,
        source,
        releases,
        patterns,
        &mut candidates,
        &mut profile_cache,
        &mut protocol_cache,
        None,
        collect_reviews.then_some(&mut processed_by_pattern),
    )
    .await;

    let pattern_map: HashMap<Uuid, watch_repo::WatchPatternRow> =
        patterns.iter().map(|p| (p.id, p.clone())).collect();

    dispatch_koblem_reviews(
        state,
        &processed_by_pattern,
        &pattern_map,
        &mut profile_cache,
        "rss",
    )
    .await;

    if !candidates.is_empty() {
        select_and_execute_winners(state, candidates, &pattern_map).await;
    }
}

/// Auto-index releases from RSS/search into the hash index (background).
/// Only indexes releases that have an info_hash (torrents).
fn auto_index_releases_to_hash_index(pool: sqlx::PgPool, releases: &[Release]) {
    use crate::parsing::parse_release;
    use crate::repositories::hash_index::{self, HashIndexEntry};

    // Regex-heavy parse over the full batch is pure CPU; run it via
    // block_in_place so it can't starve JetStream consumers on the runtime.
    let entries: Vec<HashIndexEntry> = run_cpu_bound(|| {
        releases
            .iter()
            .filter_map(|r| {
                let hash = r.info_hash.as_ref()?;
                if hash.is_empty() {
                    return None;
                }

                let parsed = parse_release(&r.title, "movies");
                let quality_json = serde_json::to_value(&parsed.quality).ok();
                let year = match &parsed.media {
                    crate::parsing::ParsedMedia::Movie(m) => {
                        m.year.and_then(|y| i16::try_from(y).ok())
                    }
                    _ => None,
                };

                let imdb_id = r
                    .external_ids
                    .iter()
                    .find(|eid| matches!(eid.source, super::ExternalIdSource::Imdb))
                    .map(|eid| eid.id.clone());

                let (season, episodes) = hash_index::derive_season_episodes(&r.title);

                Some(HashIndexEntry {
                    info_hash: hash.to_lowercase(),
                    imdb_id,
                    tmdb_id: None,
                    original_title: r.title.clone(),
                    parsed_title: Some(parsed.title),
                    year,
                    quality: quality_json,
                    size: if r.size > 0 { Some(r.size) } else { None },
                    magnet_url: r.magnet_url.clone(),
                    source: "rss".to_string(),
                    seeders: r.seeders,
                    season,
                    episodes,
                })
            })
            .collect()
    });

    if entries.is_empty() {
        return;
    }

    let count = entries.len();
    tokio::spawn(async move {
        if let Err(e) = hash_index::upsert_batch(&pool, &entries).await {
            warn!(error = %e, "Failed to auto-index RSS releases into hash index");
        } else {
            debug!(count, "Auto-indexed RSS releases into hash index");
        }
    });
}

/// Process a single release against all matching patterns.
///
/// Convenience wrapper around `process_releases` for testing and
/// single-release processing (e.g., backlog search).
pub async fn process_release(
    state: &AppState,
    source: &watch_repo::IndexerSourceRow,
    release: &Release,
    patterns: &[watch_repo::WatchPatternRow],
) {
    process_releases(state, source, std::slice::from_ref(release), patterns).await;
}

/// Resolved match configuration extracted from a WatchPatternRow.
pub struct ResolvedMatchConfig {
    pub patterns: Vec<String>,
    pub match_targets: Vec<super::types::MatchTarget>,
    pub match_mode: String,
    pub min_score: f64,
}

/// Extract match configuration from a WatchPatternRow, falling back to
/// media_want fields and augmenting with XEM scene numbering.
pub fn resolve_pattern_match_config(
    pattern: &watch_repo::WatchPatternRow,
    xem_show_map: Option<&XemShowMap>,
    scene_titles: &[SceneTitle],
) -> ResolvedMatchConfig {
    let match_mode = pattern.match_mode.as_deref().unwrap_or("fuzzy").to_string();
    let min_score = pattern.min_score.unwrap_or(0.75);

    let mut patterns = pattern.patterns.clone();
    let mut match_targets = pattern
        .match_targets
        .as_ref()
        .and_then(|v| serde_json::from_value::<Vec<super::types::MatchTarget>>(v.clone()).ok())
        .unwrap_or_default();

    if (patterns.is_empty() || match_targets.is_empty())
        && let Some(media_want) = pattern
            .media_want
            .as_ref()
            .and_then(|v| serde_json::from_value::<nats_jobs::MediaWant>(v.clone()).ok())
    {
        if patterns.is_empty() {
            patterns = search_terms_from_media_want(&media_want);
        }
        if match_targets.is_empty() {
            match_targets = match_targets_from_media_want(&media_want);
        }

        // Augment with XEM scene numbering for TV episodes
        if let Some(show_map) = xem_show_map
            && let nats_jobs::MediaWant::TvEpisode {
                show_name,
                season,
                episode,
                ..
            } = &media_want
            && let (Ok(tvdb_s), Ok(tvdb_e)) = (season.parse::<u32>(), episode.parse::<u32>())
            && let Some(scene) = show_map.scene_for_tvdb(tvdb_s, tvdb_e)
            && (scene.scene_season != tvdb_s || scene.scene_episode != tvdb_e)
        {
            match_targets.extend(xem_match_targets(
                show_name,
                scene.scene_season,
                scene.scene_episode,
                tvdb_s,
                tvdb_e,
            ));
        }

        // Augment with XEM scene numbering for TV seasons
        if let Some(show_map) = xem_show_map
            && let nats_jobs::MediaWant::TvSeason {
                show_name, season, ..
            } = &media_want
            && let Ok(tvdb_s) = season.parse::<u32>()
            && let Some(scene_season) = show_map.scene_season_for_tvdb(tvdb_s)
            && scene_season != tvdb_s
        {
            match_targets.extend(xem_season_match_targets(show_name, scene_season));
        }

        // Augment with Sonarr scene mapping titles, XEM title aliases, and the
        // provider's alternate titles
        crate::scene_titles::augment(&media_want, scene_titles, &mut patterns, &mut match_targets);
    }

    ResolvedMatchConfig {
        patterns,
        match_targets,
        match_mode,
        min_score,
    }
}

/// Match a release against a pattern from the repository
pub fn match_release_with_pattern(
    pattern: &watch_repo::WatchPatternRow,
    release_title: &str,
    xem_show_map: Option<&XemShowMap>,
    scene_titles: &[SceneTitle],
) -> Option<super::types::MatchResult> {
    let config = resolve_pattern_match_config(pattern, xem_show_map, scene_titles);

    if !config.match_targets.is_empty() {
        if let Some(result) =
            score_against_targets(release_title, &config.match_targets, config.min_score)
        {
            return Some(super::types::MatchResult {
                score: result.score,
                matched_pattern: result.matched_target,
            });
        }
        return None;
    }

    if config.patterns.is_empty() {
        return None;
    }

    match_patterns(
        &config.patterns,
        &config.match_mode,
        config.min_score,
        release_title,
    )
}

/// Clean up orphaned watch patterns whose downloads were imported but
/// `confirm_want` was never received (e.g., HTTP call from consumer failed).
///
/// For patterns without `upgrade_allowed`, deletes them outright.
/// For patterns with `upgrade_allowed`, sets an upgrade window.
async fn cleanup_orphaned_patterns(pool: &PgPool) {
    let orphans = match watch_repo::find_orphaned_patterns(pool).await {
        Ok(o) => o,
        Err(e) => {
            error!(error = %e, "Failed to find orphaned patterns");
            return;
        }
    };

    if orphans.is_empty() {
        return;
    }

    info!(
        count = orphans.len(),
        "Found orphaned watch patterns with confirmed imports"
    );

    for orphan in orphans {
        if orphan.upgrade_allowed {
            // Set an upgrade window (same as confirm_want would do)
            let defaults = match app_settings::get_upgrade_defaults(pool).await {
                Ok(d) => d,
                Err(e) => {
                    warn!(error = %e, pattern_id = %orphan.id, "Failed to load upgrade defaults for orphan cleanup");
                    continue;
                }
            };
            let window_hours = defaults.upgrade_window_hours.unwrap_or(720) as i64;
            let until = time::OffsetDateTime::now_utc() + time::Duration::hours(window_hours);
            if let Err(e) = watch_repo::set_upgrade_until(pool, orphan.id, until).await {
                warn!(error = %e, pattern_id = %orphan.id, "Failed to set upgrade window for orphan");
            } else {
                info!(
                    pattern_id = %orphan.id,
                    source_system = %orphan.source_system,
                    source_id = %orphan.source_id,
                    upgrade_until = %until,
                    "Orphan cleanup: set upgrade window for imported pattern"
                );
            }
        } else {
            // Delete the pattern
            if let Err(e) = watch_repo::delete_pattern_by_id(pool, orphan.id).await {
                warn!(error = %e, pattern_id = %orphan.id, "Failed to delete orphaned pattern");
            } else {
                info!(
                    pattern_id = %orphan.id,
                    source_system = %orphan.source_system,
                    source_id = %orphan.source_id,
                    "Orphan cleanup: deleted pattern with confirmed import"
                );
            }
        }
    }
}

/// Main watch service loop - runs periodically if there are active patterns
pub async fn run_watch_service(state: AppState) {
    loop {
        // Skip this cycle if backlog search is running
        if state
            .backlog_search_active
            .load(std::sync::atomic::Ordering::Acquire)
        {
            debug!("Watch cycle skipped: backlog search in progress");
            sleep(Duration::from_secs(10)).await;
            continue;
        }

        // Check if there are any active watch patterns
        if !has_active_patterns(&state.pool).await {
            // Nothing to watch for - sleep longer
            sleep(Duration::from_secs(60)).await;
            continue;
        }

        // If all enabled patterns were created after the last RSS check,
        // advance watermarks to now. The initial backlog search for new
        // patterns covers historical releases, so RSS doesn't need to
        // catch up from a stale watermark.
        if watch_repo::patterns_are_newer_than_last_rss(&state.pool)
            .await
            .unwrap_or(false)
        {
            info!("All watch patterns are newer than last RSS check — advancing watermarks to now");
            if let Err(e) = watch_repo::advance_all_watermarks_to_now(&state.pool).await {
                error!(error = %e, "Failed to advance watermarks");
            }
        }

        // Check if feed logging is enabled for this cycle
        let feed_logging = app_settings::get_feed_logging_enabled(&state.pool)
            .await
            .unwrap_or(false);

        // Phase 1: Fetch releases from all sources and score candidates
        let sources = get_sources_to_check(&state.pool).await;
        let mut candidates: HashMap<CandidateKey, CandidateEntry> = HashMap::new();
        // Shared caches across sources — quality profiles and protocol configs
        // are keyed by media_type_id, so they stay valid across sources.
        let mut profile_cache: HashMap<Uuid, Option<QualityProfileRow>> = HashMap::new();
        let mut protocol_cache: HashMap<Option<Uuid>, Option<String>> = HashMap::new();
        let mut all_patterns: HashMap<Uuid, watch_repo::WatchPatternRow> = HashMap::new();
        // Holds releases for koblem until the end of the cycle, so it spans every
        // source; only the ones koblem would actually be sent are kept.
        let mut processed_by_pattern: HashMap<Uuid, Vec<crate::watch::ProcessedRelease>> =
            HashMap::new();
        let collect_reviews = koblem_reviews_enabled(&state).await;

        // One collector per source when feed logging is enabled
        let mut source_collectors: Vec<DecisionCollector> = Vec::new();

        for source in &sources {
            if watch_repo::should_skip_for_search(source, &watch_repo::SearchContext::Rss) {
                continue;
            }

            let patterns = get_matching_patterns(&state.pool, source).await;

            if patterns.is_empty() {
                update_source_state(&state.pool, source.id, None, None).await;
                continue;
            }

            // Create decision collector for this source if logging is enabled
            let collector = if feed_logging {
                Some(DecisionCollector::new(
                    "feed_sync",
                    None,
                    Some(source.id),
                    false,
                ))
            } else {
                None
            };

            let fetch_start = std::time::Instant::now();
            let releases = check_indexer_source(
                &state.pool,
                &state.http_client,
                state.indexers.as_ref(),
                source,
            )
            .await;

            // Track API usage for RSS polling (counts the check, not individual pages)
            let _ = watch_repo::increment_api_calls(&state.pool, source.id).await;

            // Record the RSS fetch as a provider call
            let provider_call_id = if let Some(ref coll) = collector {
                let call_id = Uuid::new_v4();
                let duration_ms = fetch_start.elapsed().as_millis() as i32;
                let (status, results_count, error_msg) = match &releases {
                    Ok(result) => ("ok", result.releases.len() as i32, None),
                    Err(e) => ("error", 0, Some(e.to_string())),
                };
                coll.record_provider_call(LogProviderCall {
                    id: call_id,
                    indexer_source_id: Some(source.id),
                    source_name: source.name.clone(),
                    handler_type: source.handler_type.clone(),
                    search_type: Some("rss".to_string()),
                    query: None,
                    categories: None,
                    external_ids: None,
                    results_count,
                    results_after_dedup: results_count,
                    status: status.to_string(),
                    error_message: error_msg,
                    duration_ms: Some(duration_ms),
                });
                call_id
            } else {
                Uuid::nil()
            };

            match releases {
                Ok(result) => {
                    let _ = watch_repo::record_success(&state.pool, source.id).await;
                    let coll_ref = collector.as_ref().map(|c| (c, provider_call_id));
                    score_source_releases(
                        &state,
                        source,
                        &result.releases,
                        &patterns,
                        &mut candidates,
                        &mut profile_cache,
                        &mut protocol_cache,
                        coll_ref,
                        collect_reviews.then_some(&mut processed_by_pattern),
                    )
                    .await;
                    // Collect patterns for the selection phase
                    for p in patterns {
                        all_patterns.entry(p.id).or_insert(p);
                    }
                    update_source_state(&state.pool, source.id, result.new_state, None).await;
                }
                Err(e) => {
                    if let Some(ref coll) = collector {
                        coll.set_error(e.to_string());
                    }
                    // The RSS path boxes its errors; recover the indexer error so a
                    // credential rejection isn't logged as just another flaky fetch.
                    match e.downcast_ref::<crate::indexers::IndexerError>() {
                        Some(indexer_err) => {
                            crate::indexers::record_error(&state.pool, source.id, indexer_err).await
                        }
                        None => {
                            let _ =
                                watch_repo::record_failure(&state.pool, source.id, &e.to_string())
                                    .await;
                        }
                    }
                    update_source_state(&state.pool, source.id, None, Some(e.to_string())).await;
                }
            }

            if let Some(coll) = collector {
                source_collectors.push(coll);
            }
        }

        // ── Built-in hash index check (debrid RSS) ─────────────────────────
        // Only runs when a debrid download client is configured.
        if let Ok(Some(_)) =
            crate::repositories::clients::get_default_debrid_client(&state.pool).await
        {
            let watermark_json = app_settings::get(&state.pool, "hash_index_rss_watermark")
                .await
                .ok()
                .flatten();
            let watermark = watermark_json.as_ref().and_then(|v| {
                serde_json::from_value::<super::hash_index_rss::HashIndexWatermark>(v.clone()).ok()
            });

            match super::hash_index_rss::check_hash_index_rss(&state.pool, watermark.as_ref()).await
            {
                Ok((releases, new_watermark)) => {
                    if !releases.is_empty() {
                        // Score against all active patterns using a synthetic source
                        let all_pats = watch_repo::get_all_enabled_patterns(&state.pool)
                            .await
                            .unwrap_or_default();

                        let synthetic_source = synthetic_hash_index_source();
                        score_source_releases(
                            &state,
                            &synthetic_source,
                            &releases,
                            &all_pats,
                            &mut candidates,
                            &mut profile_cache,
                            &mut protocol_cache,
                            None,
                            collect_reviews.then_some(&mut processed_by_pattern),
                        )
                        .await;

                        for p in all_pats {
                            all_patterns.entry(p.id).or_insert(p);
                        }
                    }

                    // Persist watermark
                    if let Ok(json) = serde_json::to_value(&new_watermark) {
                        let _ =
                            app_settings::set(&state.pool, "hash_index_rss_watermark", &json).await;
                    }
                }
                Err(e) => {
                    warn!(error = %e, "Hash index RSS check failed");
                }
            }
        }

        // Fire-and-forget koblem reviews, one per pattern that had at least one
        // scored release. Runs regardless of whether kobload picked a winner so
        // patterns with only soft-rejected releases still get a "rescue" review.
        dispatch_koblem_reviews(
            &state,
            &processed_by_pattern,
            &all_patterns,
            &mut profile_cache,
            "rss",
        )
        .await;

        // Phase 2: Pick the best candidate per pattern+episode and execute
        let had_candidates = !candidates.is_empty();
        if had_candidates {
            // Mark winners in all collectors before executing
            for coll in &source_collectors {
                for winner in candidates.values() {
                    coll.mark_best_by_title(&winner.processed.release.title);
                }
            }
            select_and_execute_winners(&state, candidates, &all_patterns).await;
        }

        // Flush all source collectors
        for coll in source_collectors {
            coll.set_outcome(if had_candidates {
                "completed"
            } else {
                "no_matches"
            });
            decision_log::spawn_flush(state.pool.clone(), coll.into_flush_payload());
        }

        // Process expired holds (grab them now)
        process_expired_holds(&state).await;

        // Clean up orphaned patterns (imported but never confirmed)
        cleanup_orphaned_patterns(&state.pool).await;

        // Clean up expired upgrade windows
        if let Err(e) = watch_repo::delete_expired_upgrades(&state.pool).await {
            error!(error = %e, "Failed to delete expired upgrade windows");
        }

        // Sleep between checks
        sleep(Duration::from_secs(60)).await;
    }
}

/// Create a synthetic IndexerSourceRow for the built-in hash index virtual source.
/// Used to pass through the existing scoring pipeline without a real DB row.
fn synthetic_hash_index_source() -> watch_repo::IndexerSourceRow {
    watch_repo::IndexerSourceRow {
        id: Uuid::nil(),
        name: "Hash Index (built-in)".to_string(),
        handler_type: "hash_index".to_string(),
        handler_config: serde_json::json!({}),
        category_ids: None,
        media_type_ids: vec![], // Empty = matches all patterns
        enabled: true,
        last_check_at: None,
        last_error: None,
        state: None,
        created_at: time::OffsetDateTime::now_utc(),
        priority: 0,
        last_success_at: None,
        last_failure_at: None,
        consecutive_failures: 0,
        seed_ratio: None,
        seed_time_minutes: None,
        rate_limited_until: None,
        auth_failed_at: None,
        capabilities: None,
        caps_fetched_at: None,
        api_limit_daily: None,
        api_calls_today: 0,
        last_api_reset_at: time::OffsetDateTime::now_utc(),
        search_mode: "full".to_string(),
        auto_throttle: false,
        debrid_mode: "always".to_string(),
    }
}

/// Result of checking an indexer source
struct IndexerCheckResult {
    releases: Vec<Release>,
    /// Updated state to persist (watermark data)
    new_state: Option<serde_json::Value>,
}

/// Check an indexer source for new releases
///
/// Dispatches to the appropriate handler based on handler_type.
/// Returns releases and an optional updated state for watermarking.
async fn check_indexer_source(
    pool: &PgPool,
    http_client: &reqwest::Client,
    indexers: &dyn crate::indexers::IndexerResolver,
    source: &watch_repo::IndexerSourceRow,
) -> Result<IndexerCheckResult, Box<dyn std::error::Error + Send + Sync>> {
    match source.handler_type.as_str() {
        "prowlarr_rss" => {
            let source_for_rss = super::types::IndexerSource {
                id: source.id,
                name: source.name.clone(),
                handler_type: source.handler_type.clone(),
                handler_config: source.handler_config.clone(),
                category_ids: source.category_ids.clone(),
                media_type_ids: source.media_type_ids.clone(),
                enabled: source.enabled,
                last_check_at: source.last_check_at,
                last_error: source.last_error.clone(),
                state: source.state.clone(),
                created_at: source.created_at,
                priority: source.priority,
                last_success_at: source.last_success_at,
                last_failure_at: source.last_failure_at,
                consecutive_failures: source.consecutive_failures,
                seed_ratio: source.seed_ratio,
                seed_time_minutes: source.seed_time_minutes,
                rate_limited_until: source.rate_limited_until,
                auth_failed_at: source.auth_failed_at,
                capabilities: source.capabilities.clone(),
                caps_fetched_at: source.caps_fetched_at,
                api_limit_daily: source.api_limit_daily,
                api_calls_today: source.api_calls_today,
                last_api_reset_at: source.last_api_reset_at,
                search_mode: source.search_mode.clone(),
                auto_throttle: source.auto_throttle,
                debrid_mode: source.debrid_mode.clone(),
            };
            let result = super::prowlarr_rss::check_prowlarr_rss_with_watermark(
                pool,
                &source_for_rss,
                http_client,
            )
            .await?;
            let new_state = result
                .new_watermark
                .and_then(|w| serde_json::to_value(w).ok());
            Ok(IndexerCheckResult {
                releases: result.releases,
                new_state,
            })
        }
        _ => {
            // Route through generic RSS path if the handler supports it
            let handler = indexers.get(&source.handler_type);
            if let Some(h) = handler
                && h.capabilities().supports_rss
            {
                let result = super::newznab_rss::check_native_rss_with_watermark(
                    pool,
                    http_client,
                    indexers,
                    source,
                )
                .await?;
                let new_state = result
                    .new_watermark
                    .and_then(|w| serde_json::to_value(w).ok());
                Ok(IndexerCheckResult {
                    releases: result.releases,
                    new_state,
                })
            } else {
                // Handler doesn't support RSS (e.g. search-only) — skip silently
                Ok(IndexerCheckResult {
                    releases: vec![],
                    new_state: None,
                })
            }
        }
    }
}

/// Retry watch callbacks that failed previously via NATS.
///
/// Called periodically from the polling loop to retry callbacks that
/// failed due to transient NATS issues.
pub async fn retry_pending_watch_callbacks(state: &AppState) {
    if !state.nats.is_available() {
        debug!("NATS unavailable, skipping watch callback retries");
        return;
    }

    let pending_matches = match watch_repo::get_matches_pending_callback(&state.pool).await {
        Ok(matches) => matches,
        Err(e) => {
            error!(error = %e, "Failed to fetch pending watch callbacks");
            return;
        }
    };

    for watch_match in pending_matches {
        let pattern =
            match watch_repo::get_pattern_by_id(&state.pool, watch_match.watch_pattern_id).await {
                Ok(Some(p)) => p,
                Ok(None) => {
                    warn!(
                        match_id = %watch_match.id,
                        pattern_id = %watch_match.watch_pattern_id,
                        "Pattern not found for callback retry, skipping"
                    );
                    continue;
                }
                Err(e) => {
                    warn!(
                        match_id = %watch_match.id,
                        error = %e,
                        "Failed to fetch pattern for callback retry"
                    );
                    continue;
                }
            };

        let is_upgrade = watch_match.action == "upgrade";
        let replaces_download_id = watch_match
            .release_data
            .as_ref()
            .and_then(|d| d.get("replaces_download_id"))
            .and_then(|v| v.as_str())
            .and_then(|s| Uuid::parse_str(s).ok());

        let event = nats_jobs::WatchMatchedEvent {
            watch_pattern_id: watch_match.watch_pattern_id,
            source_system: pattern.source_system.clone(),
            source_id: pattern.source_id.clone(),
            match_score: watch_match.match_score,
            matched_pattern: watch_match.matched_pattern.clone(),
            release_title: watch_match.release_title.clone(),
            release_guid: watch_match.release_guid.clone(),
            download_id: watch_match.download_id,
            action: watch_match.action.clone(),
            metadata: watch_match.release_data.clone(),
            is_upgrade,
            quality_score: watch_match.quality_score,
            replaces_download_id,
            episode_key: watch_match.episode_key.clone(),
        };
        nats::publish_watch_matched(&state.nats, &event).await;

        let _ = watch_repo::mark_match_callback_sent(&state.pool, watch_match.id).await;
        info!(
            match_id = %watch_match.id,
            pattern_id = %pattern.id,
            "Watch callback retry succeeded via NATS"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::watch::ProcessedRelease;
    use nats_jobs::{RejectReason, ScoringDecision, StructuralRejectReason};
    use time::OffsetDateTime;

    fn mk_release(guid: &str, title: &str) -> Release {
        Release {
            guid: guid.to_string(),
            title: title.to_string(),
            file_name: None,
            size: 1_000_000_000,
            indexer: "test".to_string(),
            protocol: "torrent".to_string(),
            download_url: None,
            magnet_url: None,
            info_url: None,
            publish_date: "2026-04-20T00:00:00Z".to_string(),
            category_ids: Vec::new(),
            info_hash: None,
            files: None,
            seeders: Some(10),
            peers: None,
            leechers: None,
            grabs: None,
            votes: None,
            indexer_id: None,
            indexer_source_id: None,
            raw_data: serde_json::json!({}),
            external_ids: Vec::new(),
            media: None,
        }
    }

    fn mk_processed(
        release: Release,
        structural_reject: Option<StructuralRejectReason>,
    ) -> ProcessedRelease {
        let (decision, reject_reasons) = match structural_reject {
            Some(r) => (ScoringDecision::Rejected, vec![RejectReason::Structural(r)]),
            None => (ScoringDecision::Candidate, Vec::new()),
        };
        ProcessedRelease {
            release,
            decision,
            reject_reasons,
            match_score: Some(0.9),
            matched_pattern: None,
            matched_target: None,
            parsed_quality: None,
            quality_score: Some(0.5),
            quality_rejected: false,
            quality_reject_reasons: Vec::new(),
            structural_rejected: structural_reject.is_some(),
            structural_reject_reason: structural_reject,
            total_score: 1.4,
            episode_key: None,
            is_preferred_protocol: false,
            structural_bonus: 0.0,
            seeder_bonus: 0.0,
            freshness_bonus: 0.0,
            context_flags: Vec::new(),
        }
    }

    fn mk_pattern(id: Uuid, source_id: &str) -> watch_repo::WatchPatternRow {
        let now = OffsetDateTime::now_utc();
        watch_repo::WatchPatternRow {
            id,
            source_system: "test".to_string(),
            source_id: source_id.to_string(),
            callback_url: "http://localhost/cb".to_string(),
            patterns: Vec::new(),
            media_want: None,
            match_targets: None,
            match_mode: None,
            min_score: None,
            media_type_id: None,
            auto_download: true,
            metadata: None,
            enabled: true,
            upgrade_allowed: false,
            upgrade_until: None,
            created_at: now,
            updated_at: now,
            last_backlog_search_at: None,
            backlog_search_interval_hours: None,
            excluded_releases: None,
            search_not_before: None,
        }
    }

    /// Cross-pattern contamination guard: a release scored against two patterns
    /// must appear in each pattern's own slice, never mixed. This is the core
    /// fix — before the refactor, both scored rows were handed to koblem under
    /// one pattern's label.
    #[test]
    fn dispatch_plan_isolates_per_pattern_rows() {
        let euphoria_id = Uuid::new_v4();
        let wsbk_id = Uuid::new_v4();

        let release = mk_release("guid-euphoria-s03e02", "Euphoria.S03E02.1080p.WEB-DL");

        let euphoria_pr = mk_processed(release.clone(), None);
        // Same release, scored against an unrelated (WSBK) pattern — gets
        // SessionMissing because WSBK wants a SportEvent with a session type.
        let wsbk_pr = mk_processed(
            release.clone(),
            Some(StructuralRejectReason::SessionMissing),
        );

        let mut processed_by_pattern: HashMap<Uuid, Vec<ProcessedRelease>> = HashMap::new();
        processed_by_pattern.insert(euphoria_id, vec![euphoria_pr]);
        processed_by_pattern.insert(wsbk_id, vec![wsbk_pr]);

        let mut all_patterns: HashMap<Uuid, watch_repo::WatchPatternRow> = HashMap::new();
        all_patterns.insert(euphoria_id, mk_pattern(euphoria_id, "src-euphoria"));
        all_patterns.insert(wsbk_id, mk_pattern(wsbk_id, "src-wsbk"));

        let plan = koblem_dispatch_plan(&processed_by_pattern, &all_patterns);
        assert_eq!(plan.len(), 2, "one entry per pattern");

        let euphoria_entry = plan
            .iter()
            .find(|e| e.pattern.id == euphoria_id)
            .expect("euphoria entry present");
        assert_eq!(euphoria_entry.releases.len(), 1);
        assert!(
            euphoria_entry.releases[0].reject_reasons.is_empty(),
            "euphoria's slice must not contain foreign pattern's reject reasons",
        );

        let wsbk_entry = plan
            .iter()
            .find(|e| e.pattern.id == wsbk_id)
            .expect("wsbk entry present");
        assert_eq!(wsbk_entry.releases.len(), 1);
        assert_eq!(
            wsbk_entry.releases[0].structural_reject_reason,
            Some(StructuralRejectReason::SessionMissing),
            "wsbk keeps its own structural reject reason",
        );
    }

    /// Rescue case: a pattern whose only scored releases failed soft structural
    /// checks (LocationMismatch, etc.) still produces a plan entry so koblem
    /// gets a chance to second-guess kobload's strict matchers. Before the
    /// refactor this pattern would never hit publish_review because it had no
    /// kobload-accepted winner.
    #[test]
    fn dispatch_plan_includes_pattern_with_only_soft_rejects() {
        let pattern_id = Uuid::new_v4();
        let release = mk_release(
            "guid-wsbk-netherlands",
            "WSBK.2026.Netherlands.1080p.WEB-DL",
        );
        let pr = mk_processed(release, Some(StructuralRejectReason::LocationMismatch));

        let mut processed_by_pattern = HashMap::new();
        processed_by_pattern.insert(pattern_id, vec![pr]);
        let mut all_patterns = HashMap::new();
        all_patterns.insert(pattern_id, mk_pattern(pattern_id, "src-wsbk"));

        let plan = koblem_dispatch_plan(&processed_by_pattern, &all_patterns);
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].releases.len(), 1);
        // publish_review (not this helper) applies the hard-reject filter; the
        // dispatch plan passes everything through so soft-reject rescue cases
        // stay visible.
        assert!(plan[0].releases[0].structural_rejected);
    }

    /// A pattern_id present in the scored map but missing from the pattern
    /// catalogue is silently dropped — defensive against stale data.
    #[test]
    fn dispatch_plan_skips_unknown_pattern() {
        let known_id = Uuid::new_v4();
        let unknown_id = Uuid::new_v4();

        let mut processed_by_pattern = HashMap::new();
        processed_by_pattern.insert(
            known_id,
            vec![mk_processed(mk_release("g1", "Show.S01E01.WEB"), None)],
        );
        processed_by_pattern.insert(
            unknown_id,
            vec![mk_processed(mk_release("g2", "Other.S01E01.WEB"), None)],
        );

        let mut all_patterns = HashMap::new();
        all_patterns.insert(known_id, mk_pattern(known_id, "src-known"));

        let plan = koblem_dispatch_plan(&processed_by_pattern, &all_patterns);
        assert_eq!(plan.len(), 1);
        assert_eq!(plan[0].pattern.id, known_id);
    }
}
