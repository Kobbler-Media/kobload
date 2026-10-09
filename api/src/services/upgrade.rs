//! Shared upgrade eligibility logic.
//!
//! Provides consistent upgrade checking for all entry points:
//! RSS watch, backlog search, and hold expiry. Ensures the upgrade
//! margin, upgrade_until expiry, and quality comparison are applied
//! uniformly.

use sqlx::PgPool;
use tracing::debug;
use uuid::Uuid;

use crate::quality::{QualityScore, score_quality};
use crate::repositories::app_settings;
use crate::repositories::quality_profiles::QualityProfileRow;
use crate::repositories::watch as watch_repo;
use crate::trash_filters::TrashFilterSet;
use crate::watch::service::PatternDownloadState;

/// Result of an upgrade eligibility check.
pub struct UpgradeCheck {
    /// This release qualifies as an upgrade over the existing one.
    pub is_upgrade: bool,
    /// The download ID being replaced (if upgrade).
    pub replaces_download_id: Option<Uuid>,
    /// The caller should skip this release entirely.
    pub skip: bool,
    /// Reason for skipping (for debug logging).
    pub skip_reason: Option<&'static str>,
}

impl UpgradeCheck {
    fn proceed() -> Self {
        Self {
            is_upgrade: false,
            replaces_download_id: None,
            skip: false,
            skip_reason: None,
        }
    }

    fn skip(reason: &'static str) -> Self {
        Self {
            is_upgrade: false,
            replaces_download_id: None,
            skip: true,
            skip_reason: Some(reason),
        }
    }

    fn upgrade(replaces: Option<Uuid>) -> Self {
        Self {
            is_upgrade: true,
            replaces_download_id: replaces,
            skip: false,
            skip_reason: None,
        }
    }
}

/// Check upgrade eligibility for a non-season pattern (pattern-level state).
///
/// Sonarr-aligned: gating is purely quality cutoff + min_upgrade_margin.
/// No time-based "upgrade window" — once cutoff is met, only PROPER/REPACK
/// can upgrade (handled by `should_skip_for_cutoff`).
///
/// `manual=true` bypasses the upgrade-eligibility gate entirely (any client
/// asserting "this is wrong, replace it"). Active-download and library
/// fallback still apply when not in upgrade mode.
#[allow(clippy::too_many_arguments)]
pub async fn check_pattern_upgrade(
    pool: &PgPool,
    pattern_id: Uuid,
    upgrade_allowed: bool,
    manual: bool,
    dl_state: &PatternDownloadState,
    release_title: &str,
    release_publish_date: Option<&str>,
    quality_profile: Option<&QualityProfileRow>,
    trash_filters: Option<&TrashFilterSet>,
    library_quality: Option<(bool, Option<i32>)>,
) -> UpgradeCheck {
    if manual {
        return UpgradeCheck::proceed();
    }

    if dl_state.has_active {
        return UpgradeCheck::skip("pattern already has active download");
    }

    if upgrade_allowed {
        return check_upgrade_inner(
            pool,
            pattern_id,
            None, // no episode_key for pattern-level
            dl_state.has_imported,
            release_title,
            quality_profile,
            trash_filters,
        )
        .await;
    }

    // No Kobload history — check Kobbler-announced library state.
    // Pattern-level wants (Movie/SportEvent) cover a single item, so coverage is
    // all-or-nothing.
    if let Some((true, video_height)) = library_quality {
        return check_library_fallback(release_title, video_height, true);
    }

    // First grab, nothing held: hold a below-target-tier release for the grace window.
    if let Some(skip) =
        below_target_grace_skip(pool, release_title, release_publish_date, quality_profile).await
    {
        return skip;
    }

    UpgradeCheck::proceed()
}

/// Check upgrade eligibility for a TvSeason episode (per-episode state).
///
/// Same logic as pattern-level but queries per-episode DB state.
#[allow(clippy::too_many_arguments)]
pub async fn check_episode_upgrade(
    pool: &PgPool,
    pattern_id: Uuid,
    episode_key: &str,
    upgrade_allowed: bool,
    manual: bool,
    release_title: &str,
    release_publish_date: Option<&str>,
    quality_profile: Option<&QualityProfileRow>,
    trash_filters: Option<&TrashFilterSet>,
    library_quality: Option<(bool, Option<i32>)>,
) -> UpgradeCheck {
    if manual {
        return UpgradeCheck::proceed();
    }

    let ep_has_active = watch_repo::episode_has_active_download(pool, pattern_id, episode_key)
        .await
        .unwrap_or(false);
    let ep_has_imported = watch_repo::episode_has_imported_download(pool, pattern_id, episode_key)
        .await
        .unwrap_or(false);

    if ep_has_active {
        return UpgradeCheck::skip("episode already has active download");
    }

    if ep_has_imported && upgrade_allowed {
        return check_upgrade_inner(
            pool,
            pattern_id,
            Some(episode_key),
            ep_has_imported,
            release_title,
            quality_profile,
            trash_filters,
        )
        .await;
    }

    if ep_has_imported && !upgrade_allowed {
        return UpgradeCheck::skip("episode already imported");
    }

    // No Kobload history — check Kobbler-announced library state.
    // For season packs `all_in_library` may be false (the pack also fills a
    // missing episode); `check_library_fallback` still blocks quality downgrades.
    if let Some((all_in_library, video_height)) = library_quality {
        return check_library_fallback(release_title, video_height, all_in_library);
    }

    // First grab, nothing held: optionally hold a below-target-tier release for a
    // grace window so a target-tier release can appear first.
    if let Some(skip) =
        below_target_grace_skip(pool, release_title, release_publish_date, quality_profile).await
    {
        return skip;
    }

    UpgradeCheck::proceed()
}

/// Shared first-grab grace check. Returns `Some(skip)` when a below-target-tier
/// release should wait, else `None` (proceed). Reads the grace window from
/// upgrade defaults; the target tier comes from the profile's `max_resolution`.
async fn below_target_grace_skip(
    pool: &PgPool,
    release_title: &str,
    release_publish_date: Option<&str>,
    quality_profile: Option<&QualityProfileRow>,
) -> Option<UpgradeCheck> {
    let target = quality_profile.and_then(|p| p.max_resolution.as_deref());
    target?; // no target ceiling → nothing to wait for
    let grace = app_settings::get_upgrade_defaults(pool)
        .await
        .unwrap_or_default()
        .below_target_grace_minutes
        .unwrap_or(30);
    if should_wait_for_target_tier(
        release_title,
        target,
        release_publish_date,
        grace,
        time::OffsetDateTime::now_utc(),
    ) {
        debug!(
            candidate = release_title,
            target = ?target,
            grace_minutes = grace,
            "Holding below-target-tier first grab for grace window"
        );
        return Some(UpgradeCheck::skip(
            "waiting for target resolution (grace window)",
        ));
    }
    None
}

/// Library-aware fallback when Kobload has no download history but Kobbler
/// reports the file is in the library.
///
/// `all_in_library` distinguishes two cases:
/// - `true`  — every episode the release covers is already held. Only a strictly
///   higher resolution tier is worth grabbing; same/lower tier is skipped.
/// - `false` — the release (a season pack) also fills genuinely-missing episodes.
///   It should proceed UNLESS it would downgrade the quality of the held
///   episodes — a 720p pack must never replace a 1080p library.
fn check_library_fallback(
    release_title: &str,
    existing_height: Option<i32>,
    all_in_library: bool,
) -> UpgradeCheck {
    let Some(existing_h) = existing_height else {
        // In library but unprobed — we can't compare quality.
        return if all_in_library {
            UpgradeCheck::skip("in library but quality unknown, waiting for probe")
        } else {
            // Pack also fills missing episodes; don't block them on an unknown probe.
            UpgradeCheck::proceed()
        };
    };

    let new_quality = crate::quality::parse_quality(release_title);
    let candidate_height = new_quality
        .resolution
        .as_deref()
        .and_then(crate::quality::parse_resolution);

    let Some(new_h) = candidate_height else {
        // Candidate resolution unknown.
        return if all_in_library {
            UpgradeCheck::skip("in library, candidate resolution unknown")
        } else {
            UpgradeCheck::proceed()
        };
    };

    let existing_tier = crate::quality::resolution_tier(existing_h);
    let new_tier = crate::quality::resolution_tier(new_h);

    if new_tier > existing_tier {
        debug!(
            existing_height = existing_h,
            new_height = new_h,
            "Library fallback: resolution tier upgrade"
        );
        return UpgradeCheck::upgrade(None);
    }

    if new_tier < existing_tier {
        // Downgrade — never replace held episodes with lower quality, even when
        // the pack would also fill missing episodes.
        return UpgradeCheck::skip("would downgrade library quality");
    }

    // Same resolution tier.
    if all_in_library {
        UpgradeCheck::skip("in library at same resolution tier (no download history)")
    } else {
        // Pack fills missing episodes without downgrading the held ones.
        UpgradeCheck::proceed()
    }
}

/// Core upgrade comparison logic shared by pattern-level and episode-level checks.
///
/// Pure quality comparison: cutoff + min_upgrade_margin gate. No time-based
/// expiry — once cutoff is met, only PROPER/REPACK/REAL upgrades pass via
/// `should_skip_for_cutoff`.
#[allow(clippy::too_many_arguments)]
async fn check_upgrade_inner(
    pool: &PgPool,
    pattern_id: Uuid,
    episode_key: Option<&str>,
    has_imported: bool,
    release_title: &str,
    quality_profile: Option<&QualityProfileRow>,
    trash_filters: Option<&TrashFilterSet>,
) -> UpgradeCheck {
    let best_existing = if let Some(ek) = episode_key {
        watch_repo::get_best_imported_quality_for_episode(pool, pattern_id, ek)
            .await
            .ok()
            .flatten()
    } else {
        watch_repo::get_best_imported_quality_for_pattern(pool, pattern_id)
            .await
            .ok()
            .flatten()
    };

    if let Some(best_score) = best_existing {
        let new_quality = score_quality(release_title, quality_profile, None, trash_filters);
        if new_quality.rejected {
            return UpgradeCheck::skip("quality rejected");
        }

        // Tier anti-churn guard. The composite quality_score treats a same-tier
        // audio/group bump (1080p WEB-DL+DDP vs 1080p WEBRIP) and even a
        // lower-resolution release (720p WEB-DL beats 1080p WEB on score) as
        // upgrades, which re-downloads the same episode endlessly. Policy: once
        // we hold a file, an upgrade requires a genuine jump in the
        // (resolution_tier, source_tier) ordering — a higher resolution, or the
        // SAME resolution with a strictly-higher source tier (HDTV → WEB).
        // "Any WEB is acceptable", so WEB-DL vs WEBRIP is NOT an upgrade.
        // Anything not exceeding is skipped regardless of score — UNLESS the
        // candidate is a PROPER/REPACK/REAL (a genuine fix of the held release),
        // which falls through to the score/margin/cutoff gate.
        let best_title = if let Some(ek) = episode_key {
            watch_repo::get_best_imported_title_for_episode(pool, pattern_id, ek)
                .await
                .ok()
                .flatten()
        } else {
            watch_repo::get_best_imported_title_for_pattern(pool, pattern_id)
                .await
                .ok()
                .flatten()
        };
        let is_fix = new_quality.parsed.is_proper
            || new_quality.parsed.is_repack
            || new_quality.parsed.is_real;
        if let Some(best_title) = best_title.as_deref()
            && !is_fix
            && tiers_comparable(release_title, best_title)
            && !candidate_tier_exceeds(release_title, best_title)
        {
            debug!(
                existing = best_title,
                candidate = release_title,
                "Upgrade skipped: same-or-lower resolution tier (not a PROPER/REPACK)"
            );
            return UpgradeCheck::skip("same-or-lower resolution tier");
        }

        let upgrade_defaults = app_settings::get_upgrade_defaults(pool)
            .await
            .unwrap_or_default();
        let margin = upgrade_defaults.min_upgrade_margin.unwrap_or(0.10);

        if new_quality.score <= best_score + margin {
            debug!(
                new_score = new_quality.score,
                best_existing = best_score,
                margin,
                "Not a quality upgrade (below margin)"
            );
            return UpgradeCheck::skip("not a quality upgrade (below margin)");
        }

        // Cutoff: if existing quality meets the profile's cutoff, only PROPER/REPACK can upgrade
        if should_skip_for_cutoff(quality_profile, best_score, &new_quality) {
            debug!(
                best_existing = best_score,
                "Existing quality meets cutoff, skipping non-PROPER upgrade"
            );
            return UpgradeCheck::skip("existing quality meets cutoff");
        }

        let replaces = if let Some(ek) = episode_key {
            watch_repo::get_latest_imported_download_for_episode(pool, pattern_id, ek)
                .await
                .ok()
                .flatten()
        } else {
            watch_repo::get_latest_imported_download_for_pattern(pool, pattern_id)
                .await
                .ok()
                .flatten()
        };

        return UpgradeCheck::upgrade(replaces);
    }

    if has_imported {
        return UpgradeCheck::skip("already imported, no quality profile to compare upgrades");
    }

    UpgradeCheck::proceed()
}

fn resolution_tier_of(title: &str) -> Option<u8> {
    crate::quality::parse_quality(title)
        .resolution
        .as_deref()
        .and_then(crate::quality::parse_resolution)
        .map(crate::quality::resolution_tier)
}

fn source_tier_of(title: &str) -> Option<u8> {
    crate::quality::parse_quality(title)
        .source
        .as_deref()
        .and_then(crate::quality::source_tier)
}

/// Whether `candidate` is a genuine tier upgrade over `existing`, ordered by
/// (resolution_tier, source_tier) with resolution dominant:
/// - higher resolution tier → upgrade;
/// - same resolution + strictly-higher source tier → upgrade (e.g. HDTV → WEB
///   at the same resolution — a big jump worth taking);
/// - same resolution + same-or-lower source tier → NOT an upgrade ("any WEB is
///   acceptable", so WEB-DL vs WEBRIP doesn't churn);
/// - lower resolution → NOT an upgrade (downgrade).
///
/// Resolution is compared first because a higher-resolution lower-source release
/// (1080p HDTV) is still preferred over a lower-resolution higher-source one
/// (720p WEB). Returns false if the resolution tier is unknown on either side
/// (the caller gates on `tiers_comparable`, so unknowns fall through to scoring).
fn candidate_tier_exceeds(candidate_title: &str, existing_title: &str) -> bool {
    let (Some(cand_res), Some(exist_res)) = (
        resolution_tier_of(candidate_title),
        resolution_tier_of(existing_title),
    ) else {
        return false;
    };
    if cand_res != exist_res {
        return cand_res > exist_res;
    }
    // Same resolution tier — break the tie on source tier. An unknown source on
    // either side is treated as "not higher" so it doesn't trigger a re-grab.
    match (
        source_tier_of(candidate_title),
        source_tier_of(existing_title),
    ) {
        (Some(cand_src), Some(exist_src)) => cand_src > exist_src,
        _ => false,
    }
}

/// True when both titles' resolution tiers are known. The anti-churn guard only
/// fires when this holds, so an un-parseable resolution can't silently block a
/// legitimate upgrade. (Source tier may be unknown; that just means same-res
/// candidates with an unparseable source won't count as upgrades, which is the
/// safe default for anti-churn.)
fn tiers_comparable(candidate_title: &str, existing_title: &str) -> bool {
    resolution_tier_of(candidate_title).is_some() && resolution_tier_of(existing_title).is_some()
}

/// First-grab grace gate: should we WAIT before accepting a below-target-tier
/// release, hoping the target tier appears within the window?
///
/// Returns true (→ skip/wait) only when ALL hold:
/// - a target tier is known (profile `max_resolution` parses),
/// - the candidate's tier parses and is strictly below the target,
/// - `grace_minutes > 0`, and
/// - the release was published less than `grace_minutes` ago.
///
/// After the window elapses (or if publish_date is unknown/old) it returns false
/// so the below-target release is accepted — we never wait forever. This only
/// applies to FIRST grabs (no existing file); once a file exists the tier
/// anti-churn guard governs.
fn should_wait_for_target_tier(
    candidate_title: &str,
    target_max_resolution: Option<&str>,
    release_publish_date: Option<&str>,
    grace_minutes: i64,
    now: time::OffsetDateTime,
) -> bool {
    if grace_minutes <= 0 {
        return false;
    }
    let Some(target_tier) = target_max_resolution
        .and_then(crate::quality::parse_resolution)
        .map(crate::quality::resolution_tier)
    else {
        return false;
    };
    let Some(candidate_tier) = resolution_tier_of(candidate_title) else {
        return false;
    };
    if candidate_tier >= target_tier {
        return false; // at or above target — no reason to wait
    }
    // Below target: wait only if the release is younger than the grace window.
    let Some(published) = release_publish_date.and_then(parse_publish_date) else {
        return false; // unknown publish time — don't hold indefinitely
    };
    let age = now - published;
    age < time::Duration::minutes(grace_minutes)
}

/// Parse a release publish_date (RFC3339, then RFC2822, then plain ISO date).
fn parse_publish_date(s: &str) -> Option<time::OffsetDateTime> {
    use time::format_description::well_known::{Rfc2822, Rfc3339};
    time::OffsetDateTime::parse(s, &Rfc3339)
        .or_else(|_| time::OffsetDateTime::parse(s, &Rfc2822))
        .ok()
        .or_else(|| {
            let fmt = time::format_description::parse_borrowed::<2>("[year]-[month]-[day]").ok()?;
            time::Date::parse(s, &fmt)
                .ok()
                .map(|d| d.midnight().assume_utc())
        })
}

/// Returns true if the upgrade should be skipped because the existing quality
/// meets the profile's cutoff and the new release isn't a PROPER/REPACK/REAL.
fn should_skip_for_cutoff(
    quality_profile: Option<&QualityProfileRow>,
    best_score: f64,
    new_quality: &QualityScore,
) -> bool {
    if let Some(profile) = quality_profile
        && let Some(cutoff) = profile.upgrade_cutoff_score
    {
        // `upgrade_cutoff_score` is stored on the SAME raw scale as quality_score
        // (a small decimal — a complete 1080p WEB-DL ≈ 1.05, PROPER/HDR up to
        // ~1.3), NOT a 0–100 percentage. Compare directly. (Previously divided by
        // 100, which — combined with a UI capped at 100 → 0.999 — made any real
        // cutoff fire on nearly every 1080p import and silently killed upgrades.)
        best_score >= cutoff
            && !new_quality.parsed.is_proper
            && !new_quality.parsed.is_repack
            && !new_quality.parsed.is_real
    } else {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::quality::ParsedQuality;

    fn make_profile(cutoff: Option<f64>) -> QualityProfileRow {
        QualityProfileRow {
            id: uuid::Uuid::nil(),
            name: "test".to_string(),
            media_kind: "tv".to_string(),
            min_resolution: None,
            max_resolution: None,
            preferred_sources: vec![],
            rejected_sources: vec![],
            preferred_codecs: vec![],
            preferred_audio: vec![],
            preferred_languages: vec![],
            rejected_languages: vec![],
            rejected_terms: vec![],
            reject_hardcoded_subs: false,
            use_trash_filters: true,
            reject_remux: false,
            is_default: false,
            upgrade_cutoff_score: cutoff,
            created_at: time::OffsetDateTime::UNIX_EPOCH,
            updated_at: time::OffsetDateTime::UNIX_EPOCH,
        }
    }

    fn make_quality(score: f64, is_proper: bool, is_repack: bool, is_real: bool) -> QualityScore {
        QualityScore {
            score,
            rejected: false,
            reject_reasons: vec![],
            parsed: ParsedQuality {
                is_proper,
                is_repack,
                is_real,
                ..Default::default()
            },
            breakdown: Default::default(),
        }
    }

    #[test]
    fn test_cutoff_not_set_allows_upgrade() {
        let profile = make_profile(None);
        let new_q = make_quality(0.80, false, false, false);
        assert!(!should_skip_for_cutoff(Some(&profile), 0.65, &new_q));
    }

    #[test]
    fn test_cutoff_set_existing_below_allows_upgrade() {
        // Raw-score scale: cutoff 1.05, existing 1.00 < cutoff → upgrade allowed.
        let profile = make_profile(Some(1.05));
        let new_q = make_quality(1.10, false, false, false);
        assert!(!should_skip_for_cutoff(Some(&profile), 1.00, &new_q));
    }

    #[test]
    fn test_cutoff_set_existing_meets_skips_upgrade() {
        // existing 1.05 >= cutoff 1.05 → skip non-PROPER upgrade.
        let profile = make_profile(Some(1.05));
        let new_q = make_quality(1.10, false, false, false);
        assert!(should_skip_for_cutoff(Some(&profile), 1.05, &new_q));
    }

    #[test]
    fn test_cutoff_set_existing_exceeds_skips_upgrade() {
        // existing 1.08 > cutoff 1.05 → skip.
        let profile = make_profile(Some(1.05));
        let new_q = make_quality(1.20, false, false, false);
        assert!(should_skip_for_cutoff(Some(&profile), 1.08, &new_q));
    }

    #[test]
    fn test_cutoff_met_but_proper_allows_upgrade() {
        let profile = make_profile(Some(1.05));
        let new_q = make_quality(1.10, true, false, false);
        assert!(!should_skip_for_cutoff(Some(&profile), 1.08, &new_q));
    }

    #[test]
    fn test_cutoff_met_but_repack_allows_upgrade() {
        let profile = make_profile(Some(1.05));
        let new_q = make_quality(1.10, false, true, false);
        assert!(!should_skip_for_cutoff(Some(&profile), 1.08, &new_q));
    }

    #[test]
    fn test_cutoff_met_but_real_allows_upgrade() {
        let profile = make_profile(Some(1.05));
        let new_q = make_quality(1.10, false, false, true);
        assert!(!should_skip_for_cutoff(Some(&profile), 1.08, &new_q));
    }

    #[test]
    fn test_no_profile_allows_upgrade() {
        let new_q = make_quality(0.80, false, false, false);
        assert!(!should_skip_for_cutoff(None, 0.75, &new_q));
    }

    // ── check_library_fallback ───────────────────────────────────────────

    #[test]
    fn test_library_fallback_pack_downgrade_rejected() {
        // 720p pack against a 1080p library — downgrade, must skip whether or not
        // the pack also fills missing episodes.
        let title = "Daredevil.Born.Again.2025.S02.720p.x265.10bit-VRC";
        assert!(check_library_fallback(title, Some(1080), false).skip);
        assert!(check_library_fallback(title, Some(1080), true).skip);
    }

    #[test]
    fn test_library_fallback_pack_same_tier_fills_missing() {
        // 1080p pack, 1080p held, but not all covered episodes are in library
        // (the pack fills a missing finale) — proceed, it's not a downgrade.
        let title = "Show.S01.Complete.1080p.WEB-DL-GROUP";
        let check = check_library_fallback(title, Some(1080), false);
        assert!(!check.skip);
        assert!(!check.is_upgrade);
    }

    #[test]
    fn test_library_fallback_full_coverage_same_tier_skips() {
        // Every covered episode already held at 1080p, pack is 1080p — nothing to gain.
        let title = "Show.S01.Complete.1080p.WEB-DL-GROUP";
        assert!(check_library_fallback(title, Some(1080), true).skip);
    }

    #[test]
    fn test_library_fallback_upgrade_tier() {
        // 2160p pack over 1080p library — genuine tier upgrade.
        let title = "Show.S01.Complete.2160p.WEB-DL-GROUP";
        let check = check_library_fallback(title, Some(1080), true);
        assert!(check.is_upgrade);
        assert!(!check.skip);
    }

    #[test]
    fn test_library_fallback_unprobed_partial_proceeds() {
        // Held episodes unprobed (height unknown). A pack filling missing episodes
        // proceeds; a pack covering only held episodes waits for the probe.
        let title = "Show.S01.Complete.1080p.WEB-DL-GROUP";
        assert!(!check_library_fallback(title, None, false).skip);
        assert!(check_library_fallback(title, None, true).skip);
    }

    // ── (resolution, source) tier anti-churn helpers ─────────────────────
    // The guard skips when: best_title present, NOT a PROPER/REPACK/REAL,
    // tiers_comparable, and NOT candidate_tier_exceeds. These tests cover the
    // tier helpers directly; the is_fix escape is checked at the call site.

    #[test]
    fn test_tier_lower_resolution_does_not_exceed() {
        // The exact prod regression: a 720p WEB-DL out-scores a 1080p WEB on the
        // composite score, but its resolution tier is lower → not an upgrade.
        let existing = "Avatar.The.Last.Airbender.2024.S02E02.1080p.WEB.h264-ETHEL";
        let candidate =
            "Avatar.The.Last.Airbender.2024.S02E02.720p.NF.WEB-DL.DDP5.1.Atmos.H.264-playWEB";
        assert!(tiers_comparable(candidate, existing));
        assert!(!candidate_tier_exceeds(candidate, existing));
    }

    #[test]
    fn test_tier_same_res_web_variants_do_not_exceed() {
        // "Any WEB is acceptable": 1080p WEBRIP ↔ 1080p WEB-DL are the same
        // source tier → NOT an upgrade, killing the WEB↔WEB-DL churn. Both
        // directions must be non-upgrades.
        let webrip = "Show.S01E01.1080p.WEB.h264-ETHEL";
        let webdl = "Show.S01E01.1080p.WEB-DL.DDP5.1.H.264-FLUX";
        assert!(!candidate_tier_exceeds(webdl, webrip));
        assert!(!candidate_tier_exceeds(webrip, webdl));
    }

    #[test]
    fn test_tier_same_res_hdtv_to_web_exceeds() {
        // HDTV → WEB at the same resolution IS a genuine source jump → upgrade.
        let hdtv = "Show.S01E01.1080p.HDTV.x264-GROUP";
        let web = "Show.S01E01.1080p.WEB-DL.x264-GROUP";
        assert!(candidate_tier_exceeds(web, hdtv));
        // ...and the reverse (WEB → HDTV at same res) must NOT upgrade.
        assert!(!candidate_tier_exceeds(hdtv, web));
    }

    #[test]
    fn test_tier_same_res_web_to_bluray_exceeds() {
        // WEB → BluRay at the same resolution is a source upgrade.
        let web = "Show.S01E01.1080p.WEB-DL.x264-GROUP";
        let bluray = "Show.S01E01.1080p.BluRay.x264-GROUP";
        assert!(candidate_tier_exceeds(bluray, web));
    }

    #[test]
    fn test_tier_higher_resolution_exceeds_even_lower_source() {
        // Resolution dominates: 1080p HDTV beats 720p WEB (higher res wins).
        assert!(candidate_tier_exceeds(
            "Show.S01E01.1080p.HDTV.x264-GROUP",
            "Show.S01E01.720p.WEB-DL-GROUP"
        ));
        assert!(candidate_tier_exceeds(
            "Show.S01E01.2160p.WEB-DL-GROUP",
            "Show.S01E01.1080p.BluRay-GROUP"
        ));
    }

    #[test]
    fn test_tier_unknown_resolution_disables_guard() {
        // Unknown resolution on either side → tiers_comparable is false, so the
        // guard never fires and the candidate falls through to the score gate.
        let known = "Show.S01E01.1080p.WEB-DL-GROUP";
        let unknown = "Show.S01E01.WEB-DL-GROUP";
        assert!(!tiers_comparable(known, unknown));
        assert!(!tiers_comparable(unknown, known));
        assert!(!tiers_comparable(unknown, unknown));
    }

    #[test]
    fn test_tier_same_res_unknown_source_not_upgrade() {
        // Same resolution, candidate source unparseable → not an upgrade (safe
        // default; tiers_comparable is still true so the guard skips it).
        let existing = "Show.S01E01.1080p.WEB-DL-GROUP";
        let candidate = "Show.S01E01.1080p-GROUP"; // no source token
        assert!(tiers_comparable(candidate, existing));
        assert!(!candidate_tier_exceeds(candidate, existing));
    }

    // ── below-target grace window (should_wait_for_target_tier) ───────────

    fn ts(s: &str) -> time::OffsetDateTime {
        time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339).unwrap()
    }

    #[test]
    fn test_grace_holds_fresh_below_target() {
        // 720p (target 1080p) published 10 min ago → wait.
        let now = ts("2026-06-25T12:10:00Z");
        assert!(should_wait_for_target_tier(
            "Show.S01E01.720p.WEB-DL-GROUP",
            Some("1080p"),
            Some("2026-06-25T12:00:00Z"),
            30,
            now,
        ));
    }

    #[test]
    fn test_grace_accepts_after_window() {
        // Same 720p but published 45 min ago → window elapsed, accept.
        let now = ts("2026-06-25T12:45:00Z");
        assert!(!should_wait_for_target_tier(
            "Show.S01E01.720p.WEB-DL-GROUP",
            Some("1080p"),
            Some("2026-06-25T12:00:00Z"),
            30,
            now,
        ));
    }

    #[test]
    fn test_grace_ignores_at_or_above_target() {
        // A 1080p (== target) is never held, even if just published.
        let now = ts("2026-06-25T12:01:00Z");
        assert!(!should_wait_for_target_tier(
            "Show.S01E01.1080p.WEB-DL-GROUP",
            Some("1080p"),
            Some("2026-06-25T12:00:00Z"),
            30,
            now,
        ));
    }

    #[test]
    fn test_grace_disabled_or_unknown() {
        let now = ts("2026-06-25T12:05:00Z");
        // grace 0 disables the wait.
        assert!(!should_wait_for_target_tier(
            "Show.S01E01.720p.WEB-DL-GROUP",
            Some("1080p"),
            Some("2026-06-25T12:00:00Z"),
            0,
            now,
        ));
        // No target ceiling → nothing to wait for.
        assert!(!should_wait_for_target_tier(
            "Show.S01E01.720p.WEB-DL-GROUP",
            None,
            Some("2026-06-25T12:00:00Z"),
            30,
            now,
        ));
        // Unknown publish time → don't hold indefinitely.
        assert!(!should_wait_for_target_tier(
            "Show.S01E01.720p.WEB-DL-GROUP",
            Some("1080p"),
            None,
            30,
            now,
        ));
    }
}
