//! Async koblem (local LLM) review of search results via NATS.
//!
//! Takes pre-processed releases from kobload's scoring pipeline (with decisions
//! already attached) and filters to a shortlist for LLM review. No re-parsing
//! or re-scoring — uses kobload's existing results.
//!
//! Fire-and-forget: kobload publishes review requests and continues immediately.
//! Results are logged by the NATS subscriber (Phase 1) or acted on (Phase 2).

use koblem_client::types::{
    QualityProfile, ReviewCandidate, ReviewJob, ReviewRequest, WantContext,
};
use nats_jobs::MediaWant;
use tracing::{info, warn};
use uuid::Uuid;

use crate::nats::NatsState;
use crate::repositories::quality_profiles::QualityProfileRow;
use crate::watch::{ProcessedRelease, ReleaseMedia};

/// Sources that are always garbage — no LLM needed to decide.
const HARD_REJECT_SOURCES: &[&str] = &["cam", "telesync"];

/// Publish a review request to NATS from pre-processed releases.
///
/// Filters the processed releases to a shortlist:
/// - Skip: quality hard rejects (CAM/TS), wrong season/episode/title
/// - Keep: everything else including soft rejects (round/location/naming variance)
/// - Keep: no_match releases (might match via naming variance the LLM can see)
///
/// Returns the correlation ID if published, None if nothing to review.
pub async fn publish_review(
    nats: &NatsState,
    processed: &[ProcessedRelease],
    media_want: Option<&MediaWant>,
    quality_profile: Option<&QualityProfileRow>,
    source_id: &str,
    priority: &str,
) -> Option<Uuid> {
    if processed.is_empty() {
        return None;
    }

    let ctx = extract_want_context(media_want);
    let quality_profile = quality_profile.map(quality_profile_from_row);

    let mut candidates: Vec<ReviewCandidate> = Vec::new();

    for pr in processed {
        if !worth_reviewing(pr, &ctx.wanted_title) {
            continue;
        }

        let parsed_quality = pr.parsed_quality.clone().unwrap_or_default();
        let (parsed_season, parsed_episode, parsed_year) = match pr.release.media {
            Some(ReleaseMedia::Tv {
                season,
                episode,
                year,
            }) => (
                season.map(|s| s.to_string()),
                episode.map(|e| e.to_string()),
                year,
            ),
            Some(ReleaseMedia::Movie { year }) => (None, None, year),
            _ => (None, None, None),
        };

        candidates.push(ReviewCandidate {
            name: pr.release.title.clone(),
            kobload_accepted: pr.is_candidate(),
            kobload_score: pr.total_score,
            parsed_quality,
            parsed_season,
            parsed_episode,
            parsed_title: None,
            parsed_year,
            reject_reasons: pr.reject_reasons.clone(),
            quality_rejected: pr.quality_rejected,
            quality_reject_reasons: pr.quality_reject_reasons.clone(),
            structural_reject_reason: pr.structural_reject_reason,
            tier_bonus: None,
            publish_date: Some(pr.release.publish_date.clone()),
            seeders: pr.release.seeders,
            votes: pr.release.votes,
            grabs: pr.release.grabs,
        });
    }

    if candidates.is_empty() {
        return None;
    }

    let correlation_id = Uuid::new_v4();

    info!(
        %correlation_id,
        candidates = candidates.len(),
        from_releases = processed.len(),
        wanted_title = %ctx.wanted_title,
        priority,
        "Publishing review request to NATS"
    );

    let request = ReviewRequest {
        source_id: if source_id.is_empty() {
            None
        } else {
            Some(source_id.to_string())
        },
        wanted_title: ctx.wanted_title,
        want: ctx.want,
        quality_profile,
        candidates,
        sampling: None,
    };

    let job = ReviewJob {
        correlation_id,
        request,
        priority: priority.to_string(),
    };

    let payload = match serde_json::to_value(&job) {
        Ok(p) => p,
        Err(e) => {
            warn!(error = %e, "Failed to serialize ReviewJob");
            return None;
        }
    };

    nats.publisher()
        .enqueue_job("koblem", "review_candidates", payload)
        .await;

    Some(correlation_id)
}

/// The title `worth_reviewing` compares releases against for this want.
pub fn review_wanted_title(media_want: Option<&MediaWant>) -> String {
    extract_want_context(media_want).wanted_title
}

/// Whether `publish_review` would forward this release to koblem.
///
/// Callers that collect releases for a later review apply this as they score,
/// so the releases it would drop are never retained. An RSS cycle scores every
/// release against every pattern, and keeping all of those used gigabytes.
pub fn worth_reviewing(pr: &ProcessedRelease, wanted_title: &str) -> bool {
    // Garbage source quality (CAM, telesync)
    if let Some(ref pq) = pr.parsed_quality
        && let Some(ref source) = pq.source
        && HARD_REJECT_SOURCES.contains(&source.as_str())
    {
        return false;
    }

    // Hard structural rejects (code can't get these wrong)
    if let Some(reason) = pr.structural_reject_reason
        && reason.is_hard_reject()
    {
        return false;
    }

    // Quality-rejected for hard reasons (rejected_source, etc.), but keep
    // soft quality rejects (the LLM has quality checks too)
    if pr.quality_rejected
        && pr.quality_reject_reasons.iter().any(|r| {
            matches!(
                r,
                nats_jobs::QualityRejectReason::RejectedSource
                    | nats_jobs::QualityRejectReason::RejectedLanguage
            )
        })
    {
        return false;
    }

    // Keep everything else — no_match, soft rejects, candidates — unless the
    // title has zero overlap (completely unrelated content).
    has_title_overlap(&pr.release.title, wanted_title)
}

/// Minimum similarity to forward a release to koblem for LLM review.
///
/// The algorithm (`title_similarity`) produces 0.0–1.0 where:
/// - exactly-half content-word match scores ≈0.65 (e.g. "malcolm.s01e02" vs
///   "Malcolm in the Middle" — one distinctive word out of two),
/// - majority content-word match scores ≥0.7 (e.g. "malcolm.middle.s01e02"),
/// - typos / spelling variants pick up ~0.81 via per-token Jaro-Winkler.
///
/// 0.7 rejects exactly-half matches (~0.65) and lets majority / typo /
/// partial matches through (~0.8+). The LLM then decides — the filter's
/// only job is stopping noise, not gatekeeping borderline candidates.
const OVERLAP_THRESHOLD: f64 = 0.7;

/// Check if the release title is close enough to the wanted title to be
/// worth koblem's time. Uses the same Jaro-Winkler-backed `title_similarity`
/// that watch/matcher uses, with title stopwords dropped from the pattern
/// denominator so articles like "the"/"in" don't pad scores.
///
/// Falls open when the wanted title has no content tokens (e.g. "The A") —
/// there's nothing meaningful to compare against.
fn has_title_overlap(release_title: &str, wanted_title: &str) -> bool {
    let wanted_canonical = kob_normalize::canonical(wanted_title);
    let has_content = crate::services::title_tokens::title_keywords(&wanted_canonical)
        .iter()
        .any(|w| !crate::services::title_tokens::TITLE_STOPWORDS.contains(&w.as_str()));
    if !has_content {
        return true;
    }
    crate::watch::matcher::title_similarity(
        wanted_title,
        release_title,
        crate::services::title_tokens::TITLE_STOPWORDS,
    ) >= OVERLAP_THRESHOLD
}

/// Extracted from a `MediaWant`: the loose pre-filter title (used both as the
/// prompt's WANTED line and as the overlap-filter input) plus the structured
/// `WantContext` koblem renders into per-variant prompt blocks.
struct ReviewCtx {
    wanted_title: String,
    want: WantContext,
}

fn extract_want_context(media_want: Option<&MediaWant>) -> ReviewCtx {
    let Some(mw) = media_want else {
        return ReviewCtx {
            wanted_title: String::new(),
            want: WantContext::Other,
        };
    };

    match mw {
        MediaWant::TvEpisode {
            show_name,
            season,
            episode,
            year,
            air_date,
            episode_title,
            ..
        } => ReviewCtx {
            wanted_title: show_name.clone(),
            want: WantContext::TvEpisode {
                season: season.clone(),
                episode: episode.clone(),
                episode_title: episode_title.clone(),
                air_date: air_date.clone(),
                year: year.map(|y| y as i32),
            },
        },
        MediaWant::TvSeason {
            show_name,
            season,
            year,
            episodes_needed,
            ..
        } => ReviewCtx {
            wanted_title: show_name.clone(),
            want: WantContext::TvSeason {
                season: season.clone(),
                year: year.map(|y| y as i32),
                episodes_needed: episodes_needed
                    .as_ref()
                    .filter(|eps| !eps.is_empty())
                    .cloned(),
            },
        },
        MediaWant::Movie { title, year, .. } => ReviewCtx {
            wanted_title: title.clone(),
            want: WantContext::Movie {
                year: year.map(|y| y as i32),
            },
        },
        MediaWant::SportEvent {
            league_name,
            season,
            round,
            location,
            session_type,
            event_title,
            event_date,
            ..
        } => ReviewCtx {
            // Loose title for the overlap pre-filter — league + season is all
            // koblem needs to decide a release is in the ballpark. Specific
            // event info (round, location, session_type, event_title) lives in
            // the structured `WantContext::SportEvent` variant so koblem can
            // use it without gate-keeping the shortlist.
            // See feedback_koblem_loose_filter.md.
            wanted_title: format!("{} {}", league_name.trim(), season.trim())
                .trim()
                .to_string(),
            want: WantContext::SportEvent {
                league_name: league_name.clone(),
                season: season.clone(),
                round: *round,
                location: location.clone(),
                session_type: session_type.clone(),
                event_title: event_title.clone(),
                event_date: event_date.clone(),
            },
        },
        MediaWant::Book { title, .. } | MediaWant::Audiobook { title, .. } => ReviewCtx {
            wanted_title: title.clone(),
            want: WantContext::Other,
        },
        _ => ReviewCtx {
            wanted_title: String::new(),
            want: WantContext::Other,
        },
    }
}

fn quality_profile_from_row(p: &QualityProfileRow) -> QualityProfile {
    QualityProfile {
        min_resolution: p.min_resolution.clone(),
        max_resolution: p.max_resolution.clone(),
        preferred_sources: p.preferred_sources.clone(),
        preferred_codecs: p.preferred_codecs.clone(),
        preferred_audio: p.preferred_audio.clone(),
        preferred_languages: p.preferred_languages.clone(),
        rejected_sources: p.rejected_sources.clone(),
        rejected_languages: p.rejected_languages.clone(),
        rejected_terms: p.rejected_terms.clone(),
        reject_hardcoded_subs: p.reject_hardcoded_subs,
        reject_remux: p.reject_remux,
    }
}

#[cfg(test)]
mod tests {
    use super::{OVERLAP_THRESHOLD, extract_want_context, has_title_overlap};
    use koblem_client::types::WantContext;
    use nats_jobs::{EpisodeRef, MediaWant};

    fn ep(n: &str) -> EpisodeRef {
        EpisodeRef {
            season: "27".to_string(),
            episode: n.to_string(),
            sort_order: None,
            air_date: None,
            in_library: None,
            library_video_height: None,
            title: None,
        }
    }

    fn tv_season(episodes_needed: Option<Vec<EpisodeRef>>) -> MediaWant {
        MediaWant::TvSeason {
            show_name: "SVU".to_string(),
            season: "27".to_string(),
            year: Some(1999),
            episodes_needed,
            total_episodes: None,
            pack_preference: "auto".to_string(),
            last_air_date: None,
            identifiers: None,
            alternate_titles: None,
            quality_profile_id: None,
            quality_overrides: None,
        }
    }

    #[test]
    fn tv_season_with_needed_list_populates_episodes_needed() {
        let mw = tv_season(Some(vec![ep("17"), ep("18")]));
        let ctx = extract_want_context(Some(&mw));
        let WantContext::TvSeason {
            episodes_needed, ..
        } = ctx.want
        else {
            panic!("expected TvSeason variant");
        };
        let eps = episodes_needed.expect("episodes_needed should be Some");
        assert_eq!(eps.len(), 2);
        assert_eq!(eps[0].episode, "17");
        assert_eq!(eps[1].episode, "18");
    }

    #[test]
    fn tv_season_with_empty_needed_list_is_none() {
        // Empty list means "whole season" — we must not accidentally render
        // as "[episodes needed: ]" downstream.
        let mw = tv_season(Some(vec![]));
        let ctx = extract_want_context(Some(&mw));
        let WantContext::TvSeason {
            episodes_needed, ..
        } = ctx.want
        else {
            panic!("expected TvSeason variant");
        };
        assert!(episodes_needed.is_none());
    }

    #[test]
    fn tv_season_with_no_needed_list_is_none() {
        let mw = tv_season(None);
        let ctx = extract_want_context(Some(&mw));
        let WantContext::TvSeason {
            episodes_needed, ..
        } = ctx.want
        else {
            panic!("expected TvSeason variant");
        };
        assert!(episodes_needed.is_none());
    }

    #[test]
    fn overlap_rejects_unrelated_releases_for_spidey() {
        // Regression: "Spidey and His Amazing Friends" has two 7-char
        // keywords tied for longest ("amazing", "friends"). Picking a single
        // longest keyword by character length lets in releases that match
        // only the common tied word ("Raven's Home ... About Your Friends")
        // even though neither "spidey" nor "amazing" appears.
        let wanted = "Spidey and His Amazing Friends";
        let offenders = [
            "Raven#039s.Home.S03E18.What.About.Your.Friends.1080p.DSNP.WEB-DL.DDP.5.1.H.264-FLUX",
            "The.Amazing.Race.S38E05.1080p.WEB-DL",
            "Friends.S07E12.1080p.BluRay",
        ];
        for name in offenders {
            assert!(
                !has_title_overlap(name, wanted),
                "expected no overlap with {name:?}"
            );
        }
    }

    #[test]
    fn overlap_keeps_spidey_releases_naming_the_show() {
        let wanted = "Spidey and His Amazing Friends";
        assert!(has_title_overlap(
            "Spidey.and.His.Amazing.Friends.S04E18.1080p.DSNP.WEB-DL",
            wanted
        ));
        // Vague-but-close: trimmed release name that drops stopwords/pronouns
        // but still names all three content words. Both "amazing" and
        // "friends" appear, so it should pass and let koblem decide.
        assert!(has_title_overlap(
            "Spidey.Amazing.Friends.S04E18.1080p.WEB-DL",
            wanted
        ));
    }

    #[test]
    fn overlap_rejects_unrelated_releases_for_malcolm() {
        // Regression: the old substring-on-2-char-tokens check let "in"/"the"
        // match almost any release ("Manifest", NHL/MLB/Daily Show, etc.).
        // With the distinctive-token check, only releases containing the
        // longest stopword-free keyword ("malcolm") should pass.
        let wanted = "Malcolm in the Middle: Life's Still Unfair";
        let offenders = [
            "The.Real.Housewives.of.Rhode.Island.S01E02.1080p.WEB-DL",
            "The.Daily.Show.2026.04.20.Annalena.Baerbock.1080p.WEB",
            "Manifest.S03E05.Water.Landing.1080p.NF.WEB-DL",
            "MLB 2026 04 20 Athletics vs Seattle Mariners 1080p WEB",
            "90.Day.The.Single.Life.S05E11.1080p.WEB",
            "Unchosen.S01E06.FINAL.MULTI.1080p.WEB",
        ];
        for name in offenders {
            assert!(
                !has_title_overlap(name, wanted),
                "expected no overlap with {name:?}"
            );
        }
    }

    #[test]
    fn overlap_keeps_releases_that_name_the_show() {
        let wanted = "Malcolm in the Middle: Life's Still Unfair";
        assert!(has_title_overlap(
            "Malcolm.in.the.Middle.Lifes.Still.Unfair.S01E02.1080p.WEB",
            wanted
        ));
    }

    #[test]
    fn overlap_rejects_single_distinctive_word_match() {
        // Matching only "malcolm" out of [malcolm, middle] for "Malcolm in
        // the Middle" is too loose — one content word in, one out is exactly
        // half, and majority rule requires strictly more than half.
        assert!(!has_title_overlap(
            "malcolm.s01e02.1080p.web",
            "Malcolm in the Middle"
        ));
    }

    #[test]
    fn overlap_keeps_majority_word_match() {
        // Both content words ("malcolm", "middle") hit — clear signal, pass
        // to koblem.
        assert!(has_title_overlap(
            "malcolm.middle.s01e02.1080p.web",
            "Malcolm in the Middle"
        ));
    }

    #[test]
    fn overlap_keeps_spelling_variants_via_jaro_winkler() {
        // "Spider" / "Spidey" differ by one char — per-token Jaro-Winkler
        // inside title_similarity catches typos and nickname drift that
        // exact-keyword matching would miss. Pattern has 4 stopword-filtered
        // tokens [spidey, his, amazing, friends]; release hits "amazing" and
        // "friends" exact plus "spider" → "spidey" via JW.
        assert!(has_title_overlap(
            "Spider.Amazing.Friends.S04E18.1080p.WEB-DL",
            "Spidey and His Amazing Friends"
        ));
    }

    #[test]
    fn overlap_with_dot_separators_still_matches() {
        // Release-style "Title.Year.Quality" should tokenize the same as
        // space-separated. Guards against regressions in the word-boundary
        // check.
        assert!(has_title_overlap(
            "Louis.Theroux.Inside.The.Manosphere.S01E01.1080p.WEB",
            "Louis Theroux: Inside the Manosphere"
        ));
    }

    #[test]
    fn overlap_rejects_short_title_fuzzy_prefix_collision() {
        // Regression: the single-word wanted title "BEEF" was forwarded
        // to koblem against a release for a show called "Belief" because
        // the "be" Winkler prefix boost pushed JW to ≈0.91. Short pattern
        // tokens now need edit distance ≤ 1 in addition to JW ≥ 0.90.
        assert!(!has_title_overlap(
            "Before.2024.S01E07.The.Power.of.Belief.REPACK.2160p.ATVP.WEB-DL.DDP5.1.Atmos.DV.HDR10Plus.H.265-Kitsune",
            "BEEF"
        ));
    }

    #[test]
    fn overlap_with_only_stopwords_falls_open() {
        // Can't meaningfully check a title like "The A" — don't filter.
        assert!(has_title_overlap("anything.at.all.S01E01", "The A"));
    }

    #[test]
    fn overlap_sport_event_uses_loose_league_year_title() {
        // Regression from 2026-04-24 incident: the sport_event koblem title
        // must be loose ("<league> <season>") — NOT event_title — so legit
        // candidates with weird naming variants still reach koblem's LLM
        // review. Tight event_title like "Spain GP" scored 0.65 against every
        // MotoGP release and silently dropped the whole shortlist.
        //
        // This test asserts:
        //   - extract_want_context builds "MotoGP 2026" as wanted_title
        //   - has_title_overlap PASSES for both an aux-session release and
        //     the legit race, so both reach koblem for the LLM to decide
        //   - cross-sport / cross-year titles are still rejected (sanity)
        let mw = MediaWant::SportEvent {
            league_name: "MotoGP".to_string(),
            season: "2026".to_string(),
            round: Some(4),
            location: Some("Spain".to_string()),
            session_type: Some("Race".to_string()),
            event_title: Some("Spain GP".to_string()),
            quality_profile_id: None,
            quality_overrides: None,
            event_date: None,
            in_library: None,
            library_video_height: None,
        };
        let ctx = extract_want_context(Some(&mw));
        assert_eq!(ctx.wanted_title, "MotoGP 2026");
        let WantContext::SportEvent {
            event_title,
            season,
            round,
            session_type,
            location,
            ..
        } = &ctx.want
        else {
            panic!("expected SportEvent variant");
        };
        assert_eq!(event_title.as_deref(), Some("Spain GP"));
        assert_eq!(season, "2026");
        assert_eq!(*round, Some(4));
        assert_eq!(session_type.as_deref(), Some("Race"));
        assert_eq!(location.as_deref(), Some("Spain"));

        // Must accept both the bad (Press Conference) and good (Race) releases.
        for release in [
            "MotoGP.2026.Round04.Spain.Jerez.Pre-Event.Press.Conference.WEB-DL.1080p.H264.English-MWR",
            "MotoGP.2026.Round04.Spain.Jerez.Race.WEB-DL.1080p.H264.English-MWR",
            "MotoGP.2026.Round04.Espana.Jerez.Race.WEB-DL.1080p",
        ] {
            assert!(
                has_title_overlap(release, &ctx.wanted_title),
                "sport overlap filter must accept '{release}' with loose title '{}'",
                ctx.wanted_title
            );
        }

        // Cross-sport: F1 want vs MotoGP release — must NOT overlap.
        let f1_ctx_title = "Formula 1 2026";
        assert!(
            !has_title_overlap(
                "MotoGP.2026.Round04.Spain.Jerez.Race.WEB-DL.1080p",
                f1_ctx_title,
            ),
            "cross-sport overlap should be rejected"
        );

        // Cross-year: 2025 want vs 2026 release — must NOT overlap.
        let stale_ctx_title = "MotoGP 2025";
        assert!(
            !has_title_overlap(
                "MotoGP.2026.Round04.Spain.Jerez.Race.WEB-DL.1080p",
                stale_ctx_title,
            ),
            "cross-year overlap should be rejected"
        );

        // Safety: the threshold itself is still 0.7 — if it drops, a lot of
        // TV/movie overlap tests silently change behaviour. Pin it.
        assert!((OVERLAP_THRESHOLD - 0.7).abs() < f64::EPSILON);
    }

    #[test]
    fn tv_episode_does_not_populate_episodes_needed() {
        let mw = MediaWant::TvEpisode {
            show_name: "SVU".to_string(),
            season: "27".to_string(),
            episode: "17".to_string(),
            year: Some(1999),
            air_date: None,
            identifiers: None,
            alternate_titles: None,
            episode_title: None,
            quality_profile_id: None,
            quality_overrides: None,
        };
        let ctx = extract_want_context(Some(&mw));
        let WantContext::TvEpisode {
            season, episode, ..
        } = &ctx.want
        else {
            panic!("expected TvEpisode variant");
        };
        assert_eq!(season, "27");
        assert_eq!(episode, "17");
    }
}
