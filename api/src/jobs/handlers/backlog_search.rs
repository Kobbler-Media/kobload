//! Handler for backlog search of watching patterns.
//!
//! Periodically re-searches for patterns that haven't found a match yet.
//! This is a safety net for:
//! - Initial search failures (indexer was down)
//! - Indexers added after pattern creation
//! - Old catalog items that were never "new" on RSS
//!
//! Runs every 5 minutes but only processes 1-3 patterns per run to be gentle on indexers.

use crate::decision_log::{self, DecisionCollector, LogRelease};
use crate::helpers::resolve_categories_for_media_type;
use crate::indexers::IndexerContext;
use crate::jobs::JobError;
use crate::jobs::worker::JobHandler;
use crate::media::{episode_match_target, episode_search_terms};
use crate::repositories::{app_settings, watch as watch_repo};
use crate::services::search::{CascadedSearchParams, search_all_sources_cascaded};
use crate::state::AppState;
use crate::watch::episode_key;
use async_trait::async_trait;
use rand::prelude::*;
use serde_json::Value;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info, warn};
use uuid::Uuid;

/// RAII guard that clears `backlog_search_active` on drop.
struct BacklogSearchGuard(std::sync::Arc<std::sync::atomic::AtomicBool>);
impl Drop for BacklogSearchGuard {
    fn drop(&mut self) {
        self.0.store(false, std::sync::atomic::Ordering::Release);
    }
}

/// Max patterns to process per job run (be gentle on indexers)
const MAX_PATTERNS_PER_RUN: i64 = 3;

/// Default interval between backlog searches for a pattern (hours)
const DEFAULT_BACKLOG_INTERVAL_HOURS: i32 = 24;

/// Handler for backlog search jobs.
pub struct BacklogSearchHandler {
    state: AppState,
}

impl BacklogSearchHandler {
    pub fn new(state: AppState) -> Self {
        Self { state }
    }
}

#[async_trait]
impl JobHandler for BacklogSearchHandler {
    fn job_type(&self) -> &'static str {
        "backlog_search"
    }

    /// Only 1 at a time - no concurrent backlog searches
    fn max_concurrent(&self) -> usize {
        1
    }

    async fn execute(
        &self,
        _job_id: Uuid,
        _payload: Value,
        cancel: CancellationToken,
    ) -> Result<Value, JobError> {
        nats_jobs::check_cancelled(&cancel)?;

        // Signal watch service to skip its cycle while we run
        self.state
            .backlog_search_active
            .store(true, std::sync::atomic::Ordering::Release);
        let _guard = BacklogSearchGuard(self.state.backlog_search_active.clone());

        // Get patterns due for backlog search
        let patterns = get_patterns_due_for_backlog(&self.state.pool, MAX_PATTERNS_PER_RUN)
            .await
            .map_err(|e| JobError::ExecutionFailed(e.to_string()))?;

        if patterns.is_empty() {
            debug!("Backlog search skipped - no patterns due");
            return Ok(serde_json::json!({
                "patterns_checked": 0,
                "matches_found": 0
            }));
        }

        let mut patterns_checked = 0;
        let mut matches_found = 0;

        for pattern in patterns {
            // Skip if pattern already has an active download
            // (but allow upgrade-eligible patterns through — they need to evaluate quality)
            let has_active = watch_repo::pattern_has_active_download(&self.state.pool, pattern.id)
                .await
                .unwrap_or(true);

            // TvSeason patterns track per-episode state, so pattern-level has_active
            // doesn't mean every episode is covered — let them through.
            let is_season = pattern
                .media_want
                .as_ref()
                .is_some_and(episode_key::is_tv_season);

            if has_active && !is_season && !pattern.upgrade_allowed {
                debug!(
                    pattern_id = %pattern.id,
                    "Skipping backlog search - pattern has active download"
                );
                continue;
            }

            // Per-pattern serialisation: if a want / RSS / manual / research
            // trigger is already searching this pattern, skip this tick —
            // the next 5-minute cycle will retry. Non-blocking by design so
            // periodic work never queues up behind user-triggered work.
            let Some(_lock) = self.state.pattern_search_locks.try_acquire(pattern.id) else {
                debug!(
                    pattern_id = %pattern.id,
                    "Skipping backlog search - another session is already searching this pattern"
                );
                continue;
            };

            // Re-check after lock acquisition: another session may have just
            // created a download between our has_active check above and now.
            // Upgrade-eligible and season patterns are allowed through — they
            // track state at a finer grain than pattern-level has_active.
            if !(is_season || pattern.upgrade_allowed)
                && watch_repo::pattern_has_active_download(&self.state.pool, pattern.id)
                    .await
                    .unwrap_or(true)
            {
                continue;
            }

            // Perform backlog search for this pattern (scheduled auto-path)
            let found = backlog_search_pattern(&self.state, &pattern, SearchGates::AUTOMATIC).await;
            patterns_checked += 1;
            if found {
                matches_found += 1;
            }

            // Update last_backlog_search_at with jitter (0-60 min)
            let jitter_minutes = rand::rng().random_range(0..60);
            let _ =
                update_backlog_search_timestamp(&self.state.pool, pattern.id, jitter_minutes).await;
        }

        if patterns_checked > 0 {
            info!(
                patterns_checked = patterns_checked,
                matches_found = matches_found,
                "Backlog search completed"
            );
        }

        Ok(serde_json::json!({
            "patterns_checked": patterns_checked,
            "matches_found": matches_found
        }))
    }
}

/// Get patterns due for backlog search.
/// Returns patterns that:
/// - Are enabled
/// - Don't have an active download
/// - Haven't been searched recently (based on backlog_search_interval_hours)
async fn get_patterns_due_for_backlog(
    pool: &sqlx::PgPool,
    limit: i64,
) -> Result<Vec<watch_repo::WatchPatternRow>, sqlx::Error> {
    // Kept as runtime query: SELECT * with nullable column overrides would need
    // explicit column listing. WatchPatternRow has FromRow derive.
    // COALESCE(last_backlog_search_at, created_at): a freshly-created pattern
    // is treated as "just searched" via its want-time immediate search, so the
    // backlog job waits the full interval before touching it. Closes the race
    // where a want and a backlog cycle both grabbed a fresh pattern and each
    // downloaded a different release.
    sqlx::query_as::<_, watch_repo::WatchPatternRow>(
        r#"
        SELECT * FROM watch_patterns
        WHERE enabled = TRUE
          AND (search_not_before IS NULL OR search_not_before <= NOW())
          AND COALESCE(last_backlog_search_at, created_at)
              < NOW() - (COALESCE(backlog_search_interval_hours, $1) * INTERVAL '1 hour')
        ORDER BY COALESCE(last_backlog_search_at, created_at), created_at
        LIMIT $2
        "#,
    )
    .bind(DEFAULT_BACKLOG_INTERVAL_HOURS)
    .bind(limit)
    .fetch_all(pool)
    .await
}

/// Update last_backlog_search_at for a pattern, adding jitter for load spreading.
async fn update_backlog_search_timestamp(
    pool: &sqlx::PgPool,
    pattern_id: Uuid,
    jitter_minutes: i64,
) -> Result<(), sqlx::Error> {
    sqlx::query!(
        r#"
        UPDATE watch_patterns
        SET last_backlog_search_at = NOW() + ($2 * INTERVAL '1 minute')
        WHERE id = $1
        "#,
        pattern_id,
        jitter_minutes as f64,
    )
    .execute(pool)
    .await?;
    Ok(())
}

/// Convert a `ProcessedRelease` to a `LogRelease` for decision logging.
fn log_release_from_processed(
    pr: &crate::watch::ProcessedRelease,
    match_mode_str: &str,
) -> LogRelease {
    use nats_jobs::ScoringDecision;

    let decision = match pr.decision {
        ScoringDecision::Excluded => "rejected_excluded",
        ScoringDecision::Candidate => "candidate",
        ScoringDecision::NoMatch => "rejected_no_match",
        ScoringDecision::Rejected => {
            if pr.quality_rejected {
                "rejected_quality"
            } else if pr.structural_rejected {
                "rejected_structural"
            } else if pr
                .reject_reasons
                .iter()
                .any(|r| matches!(r, nats_jobs::RejectReason::CategoryMismatch))
            {
                "rejected_category"
            } else if pr
                .reject_reasons
                .iter()
                .any(|r| matches!(r, nats_jobs::RejectReason::EpisodeNotNeeded))
            {
                "rejected_episode_not_needed"
            } else {
                "rejected"
            }
        }
    };

    let decision_reason = if !pr.reject_reasons.is_empty() {
        Some(
            pr.reject_reasons
                .iter()
                .map(|r| format!("{r:?}"))
                .collect::<Vec<_>>()
                .join(", "),
        )
    } else {
        pr.structural_reject_reason
            .as_ref()
            .map(|reason| format!("{reason:?}"))
    };

    let structural_result = if pr.structural_rejected {
        Some(decision_log::StructuralResult {
            checked: true,
            matched: false,
            reject_reason: pr
                .structural_reject_reason
                .as_ref()
                .map(|r| format!("{r:?}")),
            ..Default::default()
        })
    } else {
        None
    };

    LogRelease {
        provider_call_id: None,
        release_title: pr.release.title.clone(),
        release_guid: Some(pr.release.guid.clone()),
        release_size: Some(pr.release.size),
        protocol: Some(pr.release.protocol.clone()),
        indexer_name: Some(pr.release.indexer.clone()),
        match_score: pr.match_score,
        matched_pattern: pr.matched_pattern.clone(),
        match_mode: if pr.match_score.is_some() {
            Some(match_mode_str.to_string())
        } else {
            None
        },
        quality_score: pr.quality_score,
        quality_score_breakdown: pr.quality_breakdown.clone(),
        total_score: if pr.is_candidate() {
            Some(pr.total_score)
        } else {
            None
        },
        parsed_quality: pr
            .parsed_quality
            .as_ref()
            .and_then(|q| serde_json::to_value(q).ok()),
        structural_result,
        match_explanation: pr.match_explanation.clone(),
        decision: decision.to_string(),
        decision_reason,
        episode_key: pr.episode_key.clone(),
        is_best: false,
        release_data: None,
    }
}

/// Which of the pre-search gates a caller is allowed to skip.
///
/// These used to be a single `manual: bool`, which conflated two unrelated
/// things: "this release is no better than what we hold" (upgrade eligibility)
/// and "we already have this, in flight or imported" (duplicate suppression).
/// Rejection-driven re-search needs the first bypassed and the second very much
/// enforced — collapsing them let one failed import walk a pattern down its
/// whole release list, grabbing a fresh copy after each failure and after the
/// eventual success.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SearchGates {
    /// Skip the upgrade-eligibility check, so an already-imported item can be
    /// replaced by something that does not score as an upgrade.
    pub bypass_upgrade_gate: bool,
    /// Skip the "already has an active or imported download" check. Only a
    /// human asking for this specific search should ever set this.
    pub bypass_duplicate_gate: bool,
}

impl SearchGates {
    /// Background scheduler: every gate enforced.
    pub const AUTOMATIC: Self = Self {
        bypass_upgrade_gate: false,
        bypass_duplicate_gate: false,
    };

    /// A user explicitly asked for this search ("search now"). They can see
    /// what is already downloading, so honour the request literally.
    pub const USER_REQUESTED: Self = Self {
        bypass_upgrade_gate: true,
        bypass_duplicate_gate: true,
    };

    /// Automated replacement after a failed download/import. Must be allowed to
    /// replace a non-upgrade, must NOT be allowed to duplicate work already in
    /// flight or already imported.
    pub const REPLACEMENT: Self = Self {
        bypass_upgrade_gate: true,
        bypass_duplicate_gate: false,
    };
}

/// Perform backlog search for a single pattern.
/// Returns true if a match was found.
///
/// See [`SearchGates`] for which pre-search checks each caller skips.
pub async fn backlog_search_pattern(
    state: &AppState,
    pattern: &watch_repo::WatchPatternRow,
    gates: SearchGates,
) -> bool {
    use crate::services::search::{
        SearchContext, SearchServiceContext, SearchServiceRequest, search,
    };
    use crate::services::upgrade;
    use crate::watch::service::get_pattern_download_state;
    use std::collections::HashMap;

    // Decision log: create collector if search logging is enabled
    let logging_enabled = app_settings::get_search_logging_enabled(&state.pool)
        .await
        .unwrap_or(false);
    let collector = if logging_enabled {
        Some(DecisionCollector::new(
            "backlog_search",
            Some(pattern.id),
            None,
            false,
        ))
    } else {
        None
    };

    let is_season = pattern
        .media_want
        .as_ref()
        .is_some_and(episode_key::is_tv_season);

    // Resolve match config (targets/patterns/mode/min_score) up front: the same
    // resolution supplies both the queries we send and the targets we match
    // against. Alternate show titles (Sonarr scene mappings, XEM aliases,
    // provider alternate titles) have to reach both — a show whose releases use
    // a regional name ("The Block" → "The Block AU") matches nothing without them.
    let (match_config, xem_show_map) =
        crate::watch::resolve_pattern_match_inputs(state, pattern).await;
    let min_score = match_config.min_score;

    // Custom patterns require per-term search; media_want uses a single cascaded call
    // (structured search once per source, text cascade only when needed).
    let has_custom_patterns = !pattern.patterns.is_empty();
    let search_terms = match_config.patterns.clone();

    if search_terms.is_empty() {
        return false;
    }

    // Deserialize media_want for structured Prowlarr metadata and external IDs
    let parsed_media_want = pattern
        .media_want
        .as_ref()
        .and_then(|v| serde_json::from_value::<nats_jobs::MediaWant>(v.clone()).ok());

    let external_ids = parsed_media_want
        .as_ref()
        .map(crate::media::external_ids_from_media_want)
        .unwrap_or_default();

    // Resolve quality profile for upgrade scoring
    let mut profile_cache = HashMap::new();
    let resolved_profile = if let Some(mt_id) = pattern.media_type_id {
        crate::watch::resolve_quality_profile(&state.pool, mt_id, &mut profile_cache).await
    } else {
        None
    };

    // Check if koblem review is enabled (resolved once, used for all candidates)
    let koblem_enabled = app_settings::get_koblem_settings(&state.pool)
        .await
        .unwrap_or_default()
        .enabled;

    // Resolve preferred protocol for best-candidate selection
    let protocol_config =
        crate::services::protocol::resolve_protocol_config(&state.pool, pattern.media_type_id)
            .await;
    let preferred_protocol = protocol_config
        .as_ref()
        .and_then(|c| c.preferred())
        .map(|s| s.to_string());

    // Query download state once — reused for both upfront and per-release checks
    let pattern_dl_state = if !is_season {
        Some(get_pattern_download_state(&state.pool, pattern.id).await)
    } else {
        None
    };

    // For non-season patterns, do state-level checks upfront.
    // Per-release quality comparison happens below when we have a concrete title.
    if !is_season {
        let dl_state = pattern_dl_state.as_ref().unwrap();

        // Duplicate suppression. Independent of upgrade eligibility: an
        // in-flight download is a reason to wait no matter how the candidate
        // scores, and only a human asking for this exact search may skip it.
        // Without this, failure-driven re-search grabs a replacement for a
        // download that is still running, and keeps grabbing after one lands.
        // Upgrade-eligible patterns are exempt from the has_active half: the
        // scheduler already lets them through (see BacklogSearchHandler), since
        // a better release appearing mid-download is exactly what upgrades are
        // for, and `replaces_download_id` supersedes rather than duplicates.
        if !gates.bypass_duplicate_gate {
            let dup_skip = if dl_state.has_active && !pattern.upgrade_allowed {
                Some("pattern already has an active download")
            } else if dl_state.has_imported && !pattern.upgrade_allowed {
                Some("pattern already imported and upgrades are not allowed")
            } else {
                None
            };

            if let Some(reason) = dup_skip {
                debug!(
                    pattern_id = %pattern.id,
                    reason = reason,
                    "Skipping backlog search"
                );
                return false;
            }
        }

        // Upgrade eligibility. Bypassed when we are replacing a failed grab —
        // the replacement legitimately need not out-score what failed.
        if !gates.bypass_upgrade_gate {
            let skip = if dl_state.has_active && !pattern.upgrade_allowed {
                Some("pattern already has active download")
            } else if pattern.upgrade_allowed {
                if dl_state.has_imported && resolved_profile.is_none() {
                    Some("already imported, no quality profile to compare upgrades")
                } else if dl_state.has_active && !dl_state.has_imported {
                    Some("active download in progress, no imported quality to compare against")
                } else {
                    None
                }
            } else {
                None
            };

            if let Some(reason) = skip {
                debug!(
                    pattern_id = %pattern.id,
                    reason = reason,
                    "Skipping backlog search"
                );
                return false;
            }
        }
    }

    let ctx = SearchServiceContext {
        pool: &state.pool,
        http_client: &state.http_client,
        indexers: state.indexers.as_ref(),
    };

    // Search: single cascaded call when using media_want (structured search once per
    // source, text fallback cascade only when needed). Falls back to per-term loop
    // only for custom patterns that can't use the cascade.
    let mut seen_guids = std::collections::HashSet::new();
    let mut results = Vec::new();

    if has_custom_patterns {
        // Custom patterns: loop per term (no structured search available)
        for query in &search_terms {
            if query.trim().is_empty() {
                continue;
            }

            debug!(
                pattern_id = %pattern.id,
                query = %query,
                "Running backlog search (custom pattern)"
            );

            let request = SearchServiceRequest {
                query: query.clone(),
                limit: None,
                offset: None,
                media_type_id: pattern.media_type_id,
                protocol: None,
                media_want: parsed_media_want.clone(),
                external_ids: external_ids.clone(),
                search_context: SearchContext::Backlog,
            };

            match search(&ctx, request, collector.as_ref(), None).await {
                Ok(r) => {
                    for result in r.results {
                        if seen_guids.insert(result.guid.clone()) {
                            results.push(result);
                        }
                    }
                }
                Err(e) => {
                    warn!(
                        pattern_id = %pattern.id,
                        query = %query,
                        error = %e,
                        "Backlog search failed for term"
                    );
                    continue;
                }
            }
        }
    } else {
        // Media-want derived: single call with empty query lets search() handle
        // structured search per source + text cascade, avoiding redundant API calls.
        debug!(
            pattern_id = %pattern.id,
            terms = ?search_terms,
            "Running backlog search (cascaded)"
        );

        let request = SearchServiceRequest {
            query: String::new(),
            limit: None,
            offset: None,
            media_type_id: pattern.media_type_id,
            protocol: None,
            media_want: parsed_media_want.clone(),
            external_ids: external_ids.clone(),
            search_context: SearchContext::Backlog,
        };

        // Build quality-aware early exit check: stop searching more sources once
        // we find a release that passes structural validation AND quality profile.
        let mw_for_check = parsed_media_want.as_ref();
        let profile_for_check = resolved_profile.as_ref();
        let check_fn = |results: &[crate::types::SearchResult]| -> bool {
            let Some(mw) = mw_for_check else {
                return false;
            };
            results.iter().any(|sr| {
                // Construct minimal Release for structural_reject
                let release = crate::watch::Release {
                    guid: String::new(),
                    title: sr.title.clone(),
                    file_name: None,
                    download_url: None,
                    magnet_url: None,
                    info_url: None,
                    size: sr.size,
                    publish_date: sr.publish_date.clone().unwrap_or_default(),
                    category_ids: Vec::new(),
                    indexer: String::new(),
                    protocol: sr.protocol.clone(),
                    info_hash: None,
                    files: None,
                    seeders: None,
                    peers: None,
                    leechers: None,
                    grabs: None,
                    votes: None,
                    indexer_id: None,
                    indexer_source_id: None,
                    raw_data: serde_json::Value::Null,
                    external_ids: Vec::new(),
                    media: None,
                };

                // Check 1: structural validation (year, title, round, session, etc.)
                if crate::services::want::structural_reject_from_want(
                    &release,
                    mw,
                    crate::services::want::SearchContext::TextFallback,
                )
                .is_some()
                {
                    return false;
                }

                // Check 2: quality profile (resolution, rejected sources/terms)
                let quality = crate::quality::score_quality(
                    &sr.title,
                    profile_for_check,
                    None, // no overrides for early exit check
                    None, // no trash filters for early exit check
                );
                if quality.rejected {
                    return false;
                }
                // Early exit only when quality meets upgrade cutoff (target quality).
                // Without this, a DVDRip stops the cascade when user wants 1080p.
                match profile_for_check.and_then(|p| p.upgrade_cutoff_score) {
                    Some(cutoff) => quality.score >= cutoff / 100.0,
                    None => true,
                }
            })
        };
        let early_exit: Option<crate::services::search::EarlyExitCheck<'_>> =
            if parsed_media_want.is_some() {
                Some(&check_fn)
            } else {
                None
            };

        match search(&ctx, request, collector.as_ref(), early_exit).await {
            Ok(r) => {
                for result in r.results {
                    if seen_guids.insert(result.guid.clone()) {
                        results.push(result);
                    }
                }
            }
            Err(e) => {
                warn!(
                    pattern_id = %pattern.id,
                    error = %e,
                    "Backlog search failed"
                );
            }
        }
    }

    // ── Debrid pass: search hash index if a debrid download client is configured ──
    let mut debrid_releases: Vec<crate::watch::types::Release> = Vec::new();
    if crate::repositories::clients::get_default_debrid_client(&state.pool)
        .await
        .ok()
        .flatten()
        .is_some()
    {
        debrid_releases = search_hash_index_for_pattern(
            &state.pool,
            &state.http_client,
            pattern,
            &parsed_media_want,
            &search_terms,
        )
        .await;
        // Deduplicate against traditional results
        debrid_releases.retain(|r| seen_guids.insert(Some(r.guid.clone())));

        if !debrid_releases.is_empty() {
            debug!(
                pattern_id = %pattern.id,
                count = debrid_releases.len(),
                "Backlog: found debrid candidates from hash index"
            );
        }
    }

    if results.is_empty() && debrid_releases.is_empty() {
        debug!(
            pattern_id = %pattern.id,
            "Backlog search found no results across all terms"
        );
        if let Some(c) = collector {
            c.set_outcome("no_results");
            decision_log::spawn_flush(state.pool.clone(), c.into_flush_payload());
        }
        return false;
    }

    // Resolve allowed categories for post-filtering (defense-in-depth —
    // indexers pre-filter, but some may ignore or partially respect categories)
    let allowed_categories = resolve_categories_for_media_type(&state.pool, pattern.media_type_id)
        .await
        .unwrap_or_default();

    // Parse episodes_needed once for TvSeason filtering
    let episodes_needed = if is_season {
        pattern
            .media_want
            .as_ref()
            .and_then(episode_key::get_episodes_needed)
    } else {
        None
    };

    // Early exit for TvSeason: skip search if all needed episodes are already satisfied
    // (active download or imported). Also checks SEASON_PACK coverage.
    if is_season
        && let Some(ref needed) = episodes_needed
        && !needed.is_empty()
    {
        // A prior SEASON_PACK covers everything
        let pack_active = watch_repo::episode_has_active_download(
            &state.pool,
            pattern.id,
            episode_key::SEASON_PACK_KEY,
        )
        .await
        .unwrap_or(false);
        let pack_imported = watch_repo::episode_has_imported_download(
            &state.pool,
            pattern.id,
            episode_key::SEASON_PACK_KEY,
        )
        .await
        .unwrap_or(false);

        if pack_active || pack_imported {
            debug!(
                pattern_id = %pattern.id,
                "Backlog search skipping: SEASON_PACK already active/imported"
            );
            if let Some(c) = collector {
                c.set_outcome("all_episodes_satisfied");
                decision_log::spawn_flush(state.pool.clone(), c.into_flush_payload());
            }
            return false;
        }

        // Check each individual episode
        let season_str = parsed_media_want.as_ref().and_then(|mw| match mw {
            nats_jobs::MediaWant::TvSeason { season, .. } => Some(season.as_str()),
            _ => None,
        });
        if let Some(season_str) = season_str {
            let mut all_satisfied = true;
            for ep in needed {
                let key = if let (Ok(s), Ok(e)) =
                    (season_str.parse::<u32>(), ep.episode.parse::<u32>())
                {
                    format!("S{:02}E{:02}", s, e)
                } else {
                    all_satisfied = false;
                    break;
                };
                let has_active =
                    watch_repo::episode_has_active_download(&state.pool, pattern.id, &key)
                        .await
                        .unwrap_or(false);
                let has_imported =
                    watch_repo::episode_has_imported_download(&state.pool, pattern.id, &key)
                        .await
                        .unwrap_or(false);
                // Kobbler may hold the episode without Kobload having any record
                // of it — a manual grab, a pre-existing file, or a season pack
                // imported outside the watch pipeline. Trust that only when the
                // held file is already at the profile's target resolution;
                // in-library at 720p against a 1080p profile is still wanted.
                let at_target = ep.in_library == Some(true)
                    && crate::quality::library_meets_profile_target(
                        ep.library_video_height,
                        resolved_profile
                            .as_ref()
                            .and_then(|p| p.max_resolution.as_deref()),
                    );
                if !has_active && !has_imported && !at_target {
                    all_satisfied = false;
                    break;
                }
            }
            if all_satisfied {
                debug!(
                    pattern_id = %pattern.id,
                    episodes = needed.len(),
                    "Backlog search skipping: all needed episodes already satisfied"
                );
                if let Some(c) = collector {
                    c.set_outcome("all_episodes_satisfied");
                    decision_log::spawn_flush(state.pool.clone(), c.into_flush_payload());
                }
                return false;
            }
        }
    }

    let trash_filters = state.trash_filters.get_filters();

    // Merge traditional + debrid results into a single Release vec
    let all_releases: Vec<crate::watch::types::Release> = results
        .iter()
        .map(crate::watch::types::Release::from_search_result)
        .chain(debrid_releases)
        .collect();

    // Build excluded titles from pattern.excluded_releases JSON
    let excluded_titles: std::collections::HashSet<String> = pattern
        .excluded_releases
        .as_ref()
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|entry| {
                    entry
                        .get("release_title")
                        .and_then(|v| v.as_str())
                        .filter(|t| !t.is_empty())
                        .map(|t| t.to_string())
                })
                .collect()
        })
        .unwrap_or_default();

    // Build ScoringConfig and run score_batch
    use crate::services::want::scoring::{
        ScoringConfig, SearchContext as ScoringSearchContext, score_batch,
    };
    use crate::watch::ProcessedRelease;

    let mut scoring_config = ScoringConfig::new(
        &match_config.match_targets,
        &match_config.patterns,
        match_config.min_score,
        resolved_profile.as_ref(),
        trash_filters.as_deref(),
    )
    .with_search_context(ScoringSearchContext::TextFallback)
    .with_match_mode(&match_config.match_mode)
    .with_excluded_titles(&excluded_titles)
    .with_allowed_categories(&allowed_categories);

    if let Some(ref mw) = parsed_media_want {
        scoring_config = scoring_config.with_media_want(mw);
    }
    if let Some(ref xem) = xem_show_map {
        scoring_config = scoring_config.with_xem_show_map(xem);
    }
    if let Some(ref eps) = episodes_needed
        && let Some(ref mw_json) = pattern.media_want
    {
        scoring_config = scoring_config.with_episodes_needed(eps, mw_json);
    }
    if let Some(ref proto) = preferred_protocol {
        scoring_config = scoring_config.with_preferred_protocol(proto);
    }

    let mut processed_releases = score_batch(&all_releases, &scoring_config);

    // Log all scored releases to decision collector
    let match_mode_str = if !match_config.match_targets.is_empty() {
        "match_targets"
    } else {
        "pattern"
    };
    if let Some(ref log) = collector {
        for pr in &processed_releases {
            log.record_release(log_release_from_processed(pr, match_mode_str));
        }
    }

    processed_releases.sort_by(|a, b| b.rank_cmp(a));

    // For season patterns: gather best candidate per episode_key
    // For non-season: immediate upgrade check + execute on first candidate
    let mut best_per_episode: std::collections::HashMap<String, ProcessedRelease> =
        std::collections::HashMap::new();

    for pr in &processed_releases {
        if !pr.is_candidate() {
            continue;
        }

        if is_season {
            let key = pr.episode_key.clone().unwrap_or_default();
            match best_per_episode.entry(key) {
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert(pr.clone());
                }
                std::collections::hash_map::Entry::Occupied(mut e) => {
                    if pr.is_better_than(e.get()) {
                        e.insert(pr.clone());
                    }
                }
            }
        } else {
            // Non-season: immediate upgrade check + execute on first match
            let matched_pattern_str = pr.matched_pattern.as_deref().unwrap_or_default();
            let match_score = pr.match_score.unwrap_or(0.0);

            let dl_state = pattern_dl_state.as_ref().unwrap();
            let library_quality = parsed_media_want.as_ref().and_then(|mw| match mw {
                nats_jobs::MediaWant::Movie {
                    in_library: Some(in_lib),
                    library_video_height,
                    ..
                }
                | nats_jobs::MediaWant::SportEvent {
                    in_library: Some(in_lib),
                    library_video_height,
                    ..
                } => Some((*in_lib, *library_video_height)),
                _ => None,
            });
            let uc = upgrade::check_pattern_upgrade(
                &state.pool,
                pattern.id,
                pattern.upgrade_allowed,
                gates.bypass_upgrade_gate, // upgrade bypass plumbed from caller
                dl_state,
                &pr.release.title,
                Some(&pr.release.publish_date),
                resolved_profile.as_ref(),
                trash_filters.as_deref(),
                library_quality,
            )
            .await;
            if uc.skip {
                debug!(
                    pattern_id = %pattern.id,
                    release = %pr.release.title,
                    reason = ?uc.skip_reason,
                    "Backlog search skipping release"
                );
                if let Some(ref log) = collector {
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
                        quality_score_breakdown: pr.quality_breakdown.clone(),
                        total_score: Some(pr.total_score),
                        parsed_quality: pr
                            .parsed_quality
                            .as_ref()
                            .and_then(|q| serde_json::to_value(q).ok()),
                        structural_result: None,
                        match_explanation: pr.match_explanation.clone(),
                        decision: "rejected_upgrade".to_string(),
                        decision_reason: uc.skip_reason.map(|r| r.to_string()),
                        episode_key: pr.episode_key.clone(),
                        is_best: false,
                        release_data: None,
                    });
                }
                continue;
            }

            info!(
                pattern_id = %pattern.id,
                release = %pr.release.title,
                score = match_score,
                quality_score = ?pr.quality_score,
                is_upgrade = uc.is_upgrade,
                "Backlog search matched release"
            );

            let accepted = crate::watch::handle_match_locked(
                state,
                pattern,
                &pr.release,
                match_score,
                matched_pattern_str,
                pr.release.indexer_source_id,
                pr.quality_score,
                uc.is_upgrade,
                uc.replaces_download_id,
                pr.episode_key.as_deref(),
            )
            .await;
            if accepted.is_some() {
                if let Some(ref log) = collector {
                    log.mark_best_by_title(&pr.release.title);
                    log.set_outcome("downloaded");
                    if let Some(dl_id) = accepted {
                        log.set_download_id(dl_id);
                    }
                }
                // Flush before returning
                if let Some(c) = collector {
                    decision_log::spawn_flush(state.pool.clone(), c.into_flush_payload());
                }
                return true;
            }
            // handle_match rejected (defense-in-depth) — try next candidate
            continue;
        }
    }

    // Publish pre-processed releases to koblem for LLM review
    if state.nats.is_available() && koblem_enabled {
        crate::services::want::koblem_review::publish_review(
            &state.nats,
            &processed_releases,
            parsed_media_want.as_ref(),
            resolved_profile.as_ref(),
            &pattern.source_id,
            "backlog",
        )
        .await;
    }

    // Phase 1.75 (season only): Per-episode search for missing episodes.
    // The season-level search returns newest-first, so older episodes may be missing.
    // Fire individual searches (like Sonarr) for each gap.
    if is_season
        && let Some(ref needed) = episodes_needed
        && !needed.is_empty()
        && let Some(nats_jobs::MediaWant::TvSeason {
            ref show_name,
            ref season,
            ..
        }) = parsed_media_want
    {
        let season_str = season.as_str();

        // Determine which episodes are still missing (exclude unaired episodes).
        // Once we hit an episode with a future air_date, stop — subsequent episodes
        // (including those with no air_date) are presumed to not yet be available.
        // Sort by sort_order first (episodes may be string-sorted: "1","10","2"...).
        let today = time::OffsetDateTime::now_utc().date();
        let mut sorted_needed: Vec<&nats_jobs::EpisodeRef> = needed.iter().collect();
        sorted_needed.sort_by_key(|ep| {
            ep.sort_order
                .unwrap_or(ep.episode.parse::<i32>().unwrap_or(999))
        });
        let mut hit_future_ep = false;
        let missing: Vec<&nats_jobs::EpisodeRef> =
            if best_per_episode.contains_key(episode_key::SEASON_PACK_KEY) {
                Vec::new() // Season pack covers everything
            } else {
                sorted_needed
                    .into_iter()
                    .filter(|ep| {
                        if !episode_key::episode_needs_search(ep) {
                            return false;
                        }
                        if hit_future_ep {
                            return false;
                        }
                        if ep
                            .air_date
                            .as_deref()
                            .and_then(crate::services::want::scoring::parse_iso_date)
                            .is_some_and(|d| d > today)
                        {
                            hit_future_ep = true;
                            return false;
                        }
                        let key = if let (Ok(s), Ok(e)) =
                            (season_str.parse::<u32>(), ep.episode.parse::<u32>())
                        {
                            format!("S{:02}E{:02}", s, e)
                        } else {
                            return true; // Can't determine key, assume missing
                        };
                        !best_per_episode.contains_key(&key)
                    })
                    .collect()
            };

        let found_before = best_per_episode.len();
        if !missing.is_empty() {
            info!(
                pattern_id = %pattern.id,
                missing_count = missing.len(),
                found_count = best_per_episode.len(),
                "Running per-episode fallback searches for missing episodes"
            );

            let indexer_ctx = IndexerContext {
                pool: &state.pool,
                http_client: &state.http_client,
            };

            for (i, ep_ref) in missing.iter().enumerate() {
                // Brief delay between searches to be kind to indexer APIs
                if i > 0 {
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                }

                let terms = episode_search_terms(show_name, season_str, &ep_ref.episode);
                let ep_target = episode_match_target(show_name, season_str, &ep_ref.episode);
                let ep_targets = vec![ep_target];

                // Determine structured search params
                let (s_param, e_param) =
                    (Some(season_str.to_string()), Some(ep_ref.episode.clone()));

                debug!(
                    pattern_id = %pattern.id,
                    episode = %ep_ref.episode,
                    terms = ?terms,
                    "Searching for individual episode"
                );

                let ep_releases = match search_all_sources_cascaded(&CascadedSearchParams {
                    ctx: &indexer_ctx,
                    indexers: state.indexers.as_ref(),
                    pool: &state.pool,
                    patterns: &terms,
                    limit: 50, // Unused for IndexerSearchRequest (always None now)
                    external_ids: &external_ids,
                    media_want: parsed_media_want.as_ref(),
                    media_type_id: pattern.media_type_id,
                    season: s_param.as_deref(),
                    episode: e_param.as_deref(),
                    search_context: SearchContext::Backlog,
                    early_exit_check: None,
                })
                .await
                {
                    Ok(releases) => releases,
                    Err(e) => {
                        warn!(
                            pattern_id = %pattern.id,
                            episode = %ep_ref.episode,
                            error = %e,
                            "Per-episode search failed, continuing to next"
                        );
                        continue;
                    }
                };

                // Record a synthetic provider call for the episode search
                let ep_call_id = collector.as_ref().map(|log| {
                    log.record_provider_call(crate::decision_log::LogProviderCall {
                        id: Uuid::new_v4(),
                        indexer_source_id: None,
                        source_name: format!("ep_fallback_E{:0>2}", ep_ref.episode),
                        handler_type: "cascaded".to_string(),
                        search_type: Some("structured+text".to_string()),
                        query: Some(terms.first().cloned().unwrap_or_default()),
                        categories: None,
                        external_ids: None,
                        results_count: ep_releases.len() as i32,
                        results_after_dedup: 0, // Updated below
                        status: "success".to_string(),
                        error_message: None,
                        duration_ms: None,
                    })
                });

                debug!(
                    pattern_id = %pattern.id,
                    episode = %ep_ref.episode,
                    results_count = ep_releases.len(),
                    "Per-episode search returned results"
                );

                // Score per-episode results through score_batch with episode-level targets
                let mut ep_scoring_config = ScoringConfig::new(
                    &ep_targets,
                    &[], // no pattern-based matching for episode fallback
                    min_score,
                    resolved_profile.as_ref(),
                    trash_filters.as_deref(),
                )
                .with_search_context(ScoringSearchContext::TextFallback)
                .with_excluded_titles(&excluded_titles)
                .with_allowed_categories(&allowed_categories);

                if let Some(ref mw) = parsed_media_want {
                    ep_scoring_config = ep_scoring_config.with_media_want(mw);
                }
                if let Some(ref xem) = xem_show_map {
                    ep_scoring_config = ep_scoring_config.with_xem_show_map(xem);
                }
                if let Some(ref eps) = episodes_needed
                    && let Some(ref mw_json) = pattern.media_want
                {
                    ep_scoring_config = ep_scoring_config.with_episodes_needed(eps, mw_json);
                }
                if let Some(ref proto) = preferred_protocol {
                    ep_scoring_config = ep_scoring_config.with_preferred_protocol(proto);
                }

                // Dedup episode releases before scoring
                let mut ep_seen = std::collections::HashSet::new();
                let deduped_ep_releases: Vec<_> = ep_releases
                    .iter()
                    .filter(|r| ep_seen.insert(r.guid.clone()))
                    .cloned()
                    .collect();

                let ep_scored = score_batch(&deduped_ep_releases, &ep_scoring_config);

                // Fire-and-forget koblem review for per-episode results
                if state.nats.is_available() && koblem_enabled {
                    crate::services::want::koblem_review::publish_review(
                        &state.nats,
                        &ep_scored,
                        parsed_media_want.as_ref(),
                        resolved_profile.as_ref(),
                        &pattern.source_id,
                        "backlog",
                    )
                    .await;
                }

                // Log and gather candidates
                let ep_match_mode_str = "match_targets";
                let mut ep_candidates = 0u32;
                for ep_pr in &ep_scored {
                    if let Some(ref log) = collector {
                        let mut lr = log_release_from_processed(ep_pr, ep_match_mode_str);
                        lr.provider_call_id = ep_call_id;
                        log.record_release(lr);
                    }

                    if ep_pr.is_candidate() {
                        ep_candidates += 1;
                        let key = ep_pr.episode_key.clone().unwrap_or_default();
                        match best_per_episode.entry(key) {
                            std::collections::hash_map::Entry::Vacant(e) => {
                                e.insert(ep_pr.clone());
                            }
                            std::collections::hash_map::Entry::Occupied(mut e) => {
                                if ep_pr.is_better_than(e.get()) {
                                    e.insert(ep_pr.clone());
                                }
                            }
                        }
                    }
                }

                // Update the provider call with dedup count
                if let Some(ref log) = collector
                    && let Some(call_id) = ep_call_id
                {
                    log.update_provider_call(
                        call_id,
                        ep_releases.len() as i32,
                        ep_seen.len() as i32,
                        "success",
                        None,
                        None,
                    );
                }

                debug!(
                    pattern_id = %pattern.id,
                    episode = %ep_ref.episode,
                    results = ep_releases.len(),
                    deduped = ep_seen.len(),
                    candidates = ep_candidates,
                    "Per-episode search evaluation complete"
                );
            }

            let found_after = best_per_episode.len();
            let new_found = found_after - found_before;
            info!(
                pattern_id = %pattern.id,
                episodes_before = found_before,
                episodes_after = found_after,
                new_from_fallback = new_found,
                "Per-episode fallback complete"
            );
        }
    }

    // Phase 2 (season only): Execute winners — one per episode_key
    if best_per_episode.is_empty() {
        debug!(
            pattern_id = %pattern.id,
            results_checked = results.len(),
            "Backlog search found no matches"
        );
        if let Some(c) = collector {
            c.set_outcome("no_match");
            decision_log::spawn_flush(state.pool.clone(), c.into_flush_payload());
        }
        return false;
    }

    // Remove SEASON_PACK if all needed episodes are individually covered —
    // downloading a full pack is wasteful when individual episodes already won.
    if let Some(ref needed) = episodes_needed
        && best_per_episode.contains_key(episode_key::SEASON_PACK_KEY)
    {
        let individual_keys: Vec<String> = best_per_episode
            .keys()
            .filter(|k| k.as_str() != episode_key::SEASON_PACK_KEY)
            .cloned()
            .collect();
        if episode_key::all_episodes_covered(&individual_keys, needed) {
            best_per_episode.remove(episode_key::SEASON_PACK_KEY);
            debug!(
                pattern_id = %pattern.id,
                "Removed SEASON_PACK: all needed episodes covered individually"
            );
        }
    }

    let mut found_any = false;
    for (_, winner) in best_per_episode {
        let ep_key_ref = winner.episode_key.as_deref();
        let match_score = winner.match_score.unwrap_or(0.0);
        let matched_pattern_str = winner.matched_pattern.as_deref().unwrap_or_default();

        // Run upgrade check for each winner
        let (is_upgrade, replaces_download_id) = if let Some(key) = ep_key_ref {
            let library_quality = episodes_needed
                .as_ref()
                .and_then(|eps| episode_key::check_library_coverage(key, eps));
            let uc = upgrade::check_episode_upgrade(
                &state.pool,
                pattern.id,
                key,
                pattern.upgrade_allowed,
                false, // manual: backlog auto-path
                &winner.release.title,
                Some(&winner.release.publish_date),
                resolved_profile.as_ref(),
                trash_filters.as_deref(),
                library_quality,
            )
            .await;
            if uc.skip {
                debug!(
                    pattern_id = %pattern.id,
                    episode_key = %key,
                    release = %winner.release.title,
                    reason = ?uc.skip_reason,
                    "Backlog search skipping episode winner"
                );
                continue;
            }
            (uc.is_upgrade, uc.replaces_download_id)
        } else {
            (false, None)
        };

        info!(
            pattern_id = %pattern.id,
            release = %winner.release.title,
            score = match_score,
            quality_score = ?winner.quality_score,
            is_upgrade = is_upgrade,
            episode_key = ?ep_key_ref,
            "Backlog search selected best candidate for episode"
        );

        let accepted = crate::watch::handle_match_locked(
            state,
            pattern,
            &winner.release,
            match_score,
            matched_pattern_str,
            winner.release.indexer_source_id,
            winner.quality_score,
            is_upgrade,
            replaces_download_id,
            ep_key_ref,
        )
        .await;
        if accepted.is_some() {
            if let Some(ref log) = collector {
                log.mark_best_by_title(&winner.release.title);
                if let Some(dl_id) = accepted {
                    log.set_download_id(dl_id);
                }
            }
            found_any = true;
        }
    }

    if !found_any {
        debug!(
            pattern_id = %pattern.id,
            results_checked = results.len(),
            "Backlog search found no matches after upgrade checks"
        );
    }

    // Flush collector
    if let Some(c) = collector {
        c.set_outcome(if found_any { "downloaded" } else { "no_match" });
        decision_log::spawn_flush(state.pool.clone(), c.into_flush_payload());
    }
    found_any
}

/// Search the local hash index for debrid-eligible releases matching a pattern.
/// Uses IMDB ID when available (structured lookup), falls back to title search.
/// Filters out confirmed-dead torrents (seeders = 0).
async fn search_hash_index_for_pattern(
    pool: &sqlx::PgPool,
    http_client: &reqwest::Client,
    _pattern: &watch_repo::WatchPatternRow,
    parsed_media_want: &Option<nats_jobs::MediaWant>,
    search_terms: &[String],
) -> Vec<crate::watch::types::Release> {
    use crate::repositories::hash_index;
    use crate::services::stream::{
        hash_index_rows_to_releases, hash_index_rows_with_files_to_releases,
    };

    // Extract IMDB ID and media info for external API enrichment
    let (imdb_id, is_movie, season, episode) = extract_media_info(parsed_media_want);

    // Pre-enrich: query external APIs (Torrentio) and auto-index results
    // so the subsequent hash index query has fresh data.
    if let Some(ref imdb) = imdb_id {
        enrich_hash_index_from_external(pool, http_client, imdb, is_movie, season, episode).await;
    }

    // Search the now-enriched hash index, pulling per-file data so packs
    // with a file matching the wanted S/E get emitted per-file and parent-
    // NULL rows can still be recovered via file-level metadata.
    if let Some(ref imdb) = imdb_id {
        let imdb_season = season.and_then(|s| i16::try_from(s).ok());
        let imdb_episode = episode.and_then(|e| i16::try_from(e).ok());
        match hash_index::search_by_imdb_with_files(pool, imdb, imdb_season, imdb_episode).await {
            Ok(rows) if !rows.is_empty() => {
                let filtered: Vec<_> = rows
                    .into_iter()
                    .filter(|r| !matches!(r.seeders, Some(0)))
                    .collect();
                return hash_index_rows_with_files_to_releases(filtered);
            }
            Err(e) => {
                warn!(error = %e, "Hash index IMDB search failed");
            }
            _ => {}
        }
    }

    // Fallback: title search with first non-empty term
    for term in search_terms {
        if term.trim().is_empty() {
            continue;
        }

        let year = parsed_media_want.as_ref().and_then(|mw| match mw {
            nats_jobs::MediaWant::Movie { year, .. } => year.and_then(|y| i16::try_from(y).ok()),
            _ => None,
        });

        match hash_index::search_by_title(pool, term, year).await {
            Ok(rows) if !rows.is_empty() => {
                let filtered: Vec<_> = rows
                    .into_iter()
                    .filter(|r| !matches!(r.seeders, Some(0)))
                    .collect();
                return hash_index_rows_to_releases(filtered);
            }
            Err(e) => {
                warn!(error = %e, term = %term, "Hash index title search failed");
            }
            _ => {}
        }

        // Only try first non-empty term
        break;
    }

    Vec::new()
}

/// Extract IMDB ID and media type info from parsed media_want.
fn extract_media_info(
    parsed_media_want: &Option<nats_jobs::MediaWant>,
) -> (Option<String>, bool, Option<u32>, Option<u32>) {
    let Some(mw) = parsed_media_want else {
        return (None, false, None, None);
    };

    match mw {
        nats_jobs::MediaWant::Movie { identifiers, .. } => {
            let imdb = identifiers.as_ref().and_then(|i| i.imdb_id.clone());
            (imdb, true, None, None)
        }
        nats_jobs::MediaWant::TvEpisode {
            identifiers,
            season,
            episode,
            ..
        } => {
            let imdb = identifiers.as_ref().and_then(|i| i.imdb_id.clone());
            let s = season.parse::<u32>().ok();
            let e = episode.parse::<u32>().ok();
            (imdb, false, s, e)
        }
        nats_jobs::MediaWant::TvSeason {
            identifiers,
            season,
            ..
        } => {
            let imdb = identifiers.as_ref().and_then(|i| i.imdb_id.clone());
            let s = season.parse::<u32>().ok();
            (imdb, false, s, None)
        }
        _ => (None, false, None, None),
    }
}

/// Pre-enrich the hash index by querying external APIs (Torrentio) for an IMDB ID.
/// Auto-indexes results so the subsequent hash index query has fresh data.
async fn enrich_hash_index_from_external(
    pool: &sqlx::PgPool,
    http_client: &reqwest::Client,
    imdb_id: &str,
    is_movie: bool,
    season: Option<u32>,
    episode: Option<u32>,
) {
    use crate::hash_sources::torrentio::TorrentioClient;
    use crate::repositories::hash_index;

    let client = TorrentioClient::new(http_client.clone());
    let results = if is_movie {
        client.search_movie(imdb_id).await
    } else if let (Some(s), Some(e)) = (season, episode) {
        client.search_series(imdb_id, s, e).await
    } else if let Some(s) = season {
        // Season search: try episode 1 to get season pack results
        client.search_series(imdb_id, s, 1).await
    } else {
        return;
    };

    if results.is_empty() {
        return;
    }

    let entries: Vec<_> = results.iter().map(|r| r.entry.clone()).collect();
    let files: Vec<_> = results.iter().filter_map(|r| r.file.clone()).collect();

    info!(
        imdb_id = %imdb_id,
        entries = entries.len(),
        files = files.len(),
        "Debrid backlog: enriched hash index from Torrentio"
    );

    if let Err(e) = hash_index::upsert_batch(pool, &entries).await {
        warn!(error = %e, "Failed to auto-index Torrentio results");
    }

    if !files.is_empty()
        && let Err(e) = hash_index::upsert_files(pool, &files).await
    {
        warn!(error = %e, "Failed to auto-index Torrentio file entries");
    }
}
