use axum::http::StatusCode;
use sqlx::PgPool;
use tracing::warn;
use uuid::Uuid;

use crate::decision_log::{self, DecisionCollector};
use crate::repositories::{app_settings, watch as watch_repo};
use crate::watch::episode_key::{SEASON_PACK_KEY, key_covers_episode};

use super::search::do_immediate_search;
use super::types::{WantServiceContext, WantServiceError, WantServiceRequest, WantServiceResult};
use nats_jobs::{MediaWant, WantStatus};

pub async fn handle_want(
    ctx: &WantServiceContext<'_>,
    mut request: WantServiceRequest,
) -> Result<WantServiceResult, WantServiceError> {
    let pattern = create_or_update_watch_pattern(ctx.pool, &request)
        .await
        .map_err(|e| WantServiceError::new(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let watch_pattern_id = pattern.id;

    request.watch_pattern_id = Some(watch_pattern_id);
    if let Some(want) = request.media_want.as_mut() {
        crate::learned_aliases::apply(ctx.pool, want).await;
    }
    // The client only sends the exclusions it knows about; the pattern also holds
    // the releases Kobload watched fail, merged in by the upsert above.
    request.excluded_releases = pattern.excluded_releases;

    // Serialise against any other trigger (backlog_search, RSS feed_sync, manual,
    // research re-search) that might also be searching this pattern right now.
    // Blocking: user-triggered, must not drop the work. Lock is held across the
    // full search + dispatch so concurrent callers see our created download via
    // the post-lock coverage fast-path below.
    let _lock = ctx.pattern_search_locks.acquire(watch_pattern_id).await;

    // Fast-path: if every target this request asks for is already covered by an
    // active download on this pattern, don't dispatch a duplicate search.
    // Upgrade patterns are exempt — they may legitimately re-download for quality bumps.
    if !request.upgrade_allowed
        && !request.manual
        && request_already_covered(ctx.pool, watch_pattern_id, &request).await
    {
        return Ok(WantServiceResult {
            watch_pattern_id,
            status: WantStatus::Watching,
            download_id: None,
            selected_release: None,
            results_found: 0,
            best_score: 0.0,
            matched_pattern: None,
            matched_target: None,
            episodes_downloaded: Vec::new(),
        });
    }

    // Decision log: create collector if search logging is enabled
    let logging_enabled = app_settings::get_search_logging_enabled(ctx.pool)
        .await
        .unwrap_or(false);
    let collector = if logging_enabled {
        Some(DecisionCollector::new(
            "want",
            Some(watch_pattern_id),
            None,
            false,
        ))
    } else {
        None
    };

    let immediate = match do_immediate_search(ctx, &request, collector.as_ref()).await {
        Ok(result) => result,
        Err(err) => {
            warn!(error = %err, "Immediate search failed, falling back to watching");
            if let Some(ref log) = collector {
                log.set_outcome("error");
                log.set_error(err.to_string());
            }
            super::search::ImmediateSearchResult {
                download_id: None,
                selected_release: None,
                results_found: 0,
                best_score: 0.0,
                matched_pattern: None,
                matched_target: None,
                status: nats_jobs::WantStatus::Watching,
                protocol_held: false,
                episodes_downloaded: Vec::new(),
            }
        }
    };

    // Record outcome in decision log
    if let Some(ref log) = collector {
        let outcome = if immediate.download_id.is_some() {
            "downloaded"
        } else if immediate.protocol_held {
            "held"
        } else if immediate.results_found == 0 {
            "no_results"
        } else {
            "no_match"
        };
        log.set_outcome(outcome);
        if let Some(dl_id) = immediate.download_id {
            log.set_download_id(dl_id);
        }
        if let Some(ref sel) = immediate.selected_release {
            log.mark_best_by_title(&sel.release.title);
        }
    }

    // Flush collector
    if let Some(c) = collector {
        decision_log::spawn_flush(ctx.pool.clone(), c.into_flush_payload());
    }

    Ok(WantServiceResult {
        watch_pattern_id,
        status: immediate.status,
        download_id: immediate.download_id,
        selected_release: immediate.selected_release.clone(),
        results_found: immediate.results_found,
        best_score: immediate.best_score,
        matched_pattern: immediate.matched_pattern,
        matched_target: immediate.matched_target,
        episodes_downloaded: immediate.episodes_downloaded,
    })
}

/// Returns true if every target this request is asking for is already covered
/// by an active (non-failed, not-yet-imported) download on the pattern.
///
/// For multi-target patterns (currently `TvSeason` with `episodes_needed`),
/// "covered" means each requested episode is satisfied by some active match —
/// either a `SEASON_PACK` or a single/range key that includes the episode
/// number. A pattern-level "any active download" check is wrong here: one
/// episode's download doesn't speak for the rest.
///
/// For single-target patterns (movie, book, single TvEpisode), the check
/// degenerates to the historical "pattern has any active download".
async fn request_already_covered(
    pool: &PgPool,
    pattern_id: Uuid,
    request: &WantServiceRequest,
) -> bool {
    let needed = match request.media_want.as_ref() {
        Some(MediaWant::TvSeason {
            episodes_needed: Some(eps),
            ..
        }) if !eps.is_empty() => eps,
        _ => {
            return watch_repo::pattern_has_active_download(pool, pattern_id)
                .await
                .unwrap_or(false);
        }
    };

    let active_keys = match watch_repo::active_episode_keys(pool, pattern_id).await {
        Ok(keys) => keys,
        Err(e) => {
            warn!(error = %e, "active_episode_keys query failed; not fast-pathing");
            return false;
        }
    };

    if active_keys.iter().any(|k| k == SEASON_PACK_KEY) {
        return true;
    }

    needed.iter().all(|ep| {
        active_keys
            .iter()
            .any(|k| key_covers_episode(k, &ep.episode))
    })
}

async fn create_or_update_watch_pattern(
    pool: &PgPool,
    request: &WantServiceRequest,
) -> Result<watch_repo::WatchPatternRow, String> {
    let input = watch_repo::WatchPatternSourceInput {
        source_system: request.source_system.clone(),
        source_id: request.source_id.clone(),
        callback_url: Some(request.callback_url.clone()),
        patterns: request.patterns.clone(),
        media_want: request
            .media_want
            .as_ref()
            .and_then(|v| serde_json::to_value(v).ok()),
        match_targets: if request.match_targets.is_empty() {
            None
        } else {
            Some(serde_json::to_value(&request.match_targets).unwrap_or_default())
        },
        match_mode: Some(request.match_mode.clone()),
        min_score: Some(request.min_score),
        media_type_id: request.media_type_id,
        auto_download: request.auto_download,
        metadata: request.metadata.clone(),
        enabled: true,
        upgrade_allowed: request.upgrade_allowed,
        excluded_releases: request.excluded_releases.clone(),
        search_not_before: request.search_not_before,
    };

    let exists = watch_repo::pattern_exists(pool, &request.source_system, &request.source_id)
        .await
        .map_err(|e| e.to_string())?;

    if exists {
        watch_repo::update_pattern_by_source(pool, &input)
            .await
            .map_err(|e| e.to_string())?;
    } else {
        watch_repo::insert_pattern_by_source(pool, &input)
            .await
            .map_err(|e| e.to_string())?;
    }

    watch_repo::get_pattern_by_source(pool, &request.source_system, &request.source_id)
        .await
        .map_err(|e| e.to_string())?
        .ok_or_else(|| "Pattern not found after creation".to_string())
}
