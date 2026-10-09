mod downloads;
mod episode_fallback;
pub(crate) mod koblem_review;
pub(crate) mod scoring;
mod search;
mod service;
mod types;

pub use scoring::{
    ScoringConfig, SearchContext, freshness_tiebreaker, group_by_target_id, score_batch,
    structural_bonus, structural_reject, structural_reject_from_want, votes_reject,
};
pub use service::handle_want;
pub use types::{WantServiceContext, WantServiceError, WantServiceRequest, WantServiceResult};

use crate::repositories::quality_profiles::QualityProfileRow;
use crate::repositories::watch as watch_repo;
use crate::services::upgrade;
use crate::trash_filters::TrashFilterSet;
use crate::watch::ProcessedRelease;
use crate::watch::episode_key;
use nats_jobs::EpisodeRef;
use tracing::{debug, warn};
use uuid::Uuid;

/// Reserve a watch_match row atomically BEFORE creating a download.
///
/// Returns `Some(Some(match_id))` if the match was created (we won the race),
/// `Some(None)` if there's no watch_pattern_id (no match needed, proceed anyway),
/// `None` if another thread already matched this release.
///
/// Caller invariant: every reserved row must transition to a final state.
/// On success, call `update_match_download_id`. On any non-success branch
/// (skip, create_download Err, etc.), call `update_match_skipped` so the
/// row doesn't sit forever as `action='downloaded' AND download_id IS NULL`.
///
/// `manual` additionally reclaims a row an earlier automatic pass parked as
/// `skipped`. Without that, a person asking for another copy of something the
/// backlog has already swept is blocked on every candidate it looked at.
pub(super) async fn reserve_watch_match(
    pool: &sqlx::PgPool,
    watch_pattern_id: Option<Uuid>,
    best: &ProcessedRelease,
    episode_key: Option<&str>,
    manual: bool,
) -> Option<Option<Uuid>> {
    let Some(watch_pattern_id) = watch_pattern_id else {
        return Some(None);
    };
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

    let match_input = watch_repo::WatchMatchInput {
        pattern_id: watch_pattern_id,
        indexer_source_id: best.release.indexer_source_id,
        release_guid: &best.release.guid,
        release_title: &best.release.title,
        release_data: Some(&release_data),
        match_score: best.total_score,
        matched_pattern: best.matched_pattern.as_deref().unwrap_or(""),
        action: "downloaded",
        download_id: None,
        hold_until: None,
        held: false,
        quality_score: best.quality_score,
        rejected: false,
        rejected_at: None,
        rejected_reason: None,
        episode_key,
    };
    let reserved = match manual {
        true => watch_repo::reclaim_skipped_match(pool, &match_input).await,
        false => watch_repo::create_match_if_not_exists(pool, &match_input).await,
    };
    match reserved {
        Ok(Some(match_id)) => Some(Some(match_id)),
        Ok(None) => None,
        Err(e) => {
            warn!(
                pattern_id = %watch_pattern_id,
                error = %e,
                "Failed to reserve watch_match, proceeding without dedup"
            );
            Some(None)
        }
    }
}

/// Context for episode upgrade checks in the want handler.
///
/// Resolved once at the top of `do_immediate_search` and threaded through
/// to avoid repeated DB queries. `manual` carries the caller's assertion that a
/// person asked for this now — a chat regrab or a UI re-grab — which is the one
/// thing that makes an already-imported episode a candidate again.
pub(super) struct EpisodeUpgradeContext {
    pub episodes_needed: Option<Vec<EpisodeRef>>,
    pub upgrade_allowed: bool,
    pub manual: bool,
    pub pattern_id: Option<Uuid>,
}

/// Check whether an episode download should be skipped due to library coverage
/// or upgrade ineligibility. Returns `Some(reason)` to skip, `None` to proceed.
#[allow(clippy::too_many_arguments)]
pub(super) async fn should_skip_episode(
    pool: &sqlx::PgPool,
    episode_key: &str,
    release_title: &str,
    release_publish_date: Option<&str>,
    uc: &EpisodeUpgradeContext,
    quality_profile: Option<&QualityProfileRow>,
    trash_filters: Option<&std::sync::Arc<TrashFilterSet>>,
) -> Option<String> {
    let episodes_needed = uc.episodes_needed.as_ref()?;
    let pattern_id = uc.pattern_id?;

    let library_quality = episode_key::check_library_coverage(episode_key, episodes_needed);
    let check = upgrade::check_episode_upgrade(
        pool,
        pattern_id,
        episode_key,
        uc.upgrade_allowed,
        uc.manual,
        release_title,
        release_publish_date,
        quality_profile,
        trash_filters.map(|a| a.as_ref()),
        library_quality,
    )
    .await;

    if check.skip {
        let reason = check.skip_reason.unwrap_or("upgrade check: skip");
        debug!(
            episode_key,
            release = release_title,
            reason,
            "Want handler: skipping episode (upgrade/library check)"
        );
        Some(reason.to_string())
    } else {
        None
    }
}
