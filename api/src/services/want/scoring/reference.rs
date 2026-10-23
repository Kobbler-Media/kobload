//! `score_release` as it was before release-only work was shared across the
//! patterns of a fetch. The shared path has to score every pair identically.

use super::*;
use crate::quality::{parse_quality, score_quality};

pub fn score_release(release: &Release, config: &ScoringConfig) -> ProcessedRelease {
    use nats_jobs::{RejectReason, ScoringDecision};

    // ── Gate 1: Exclusion (hard early return) ──
    if let Some(excluded) = config.excluded_titles
        && excluded.contains(&release.title)
    {
        return ProcessedRelease {
            release: release.clone().into(),
            decision: ScoringDecision::Excluded,
            reject_reasons: vec![],
            match_score: None,
            matched_pattern: None,
            matched_target: None,
            parsed_quality: Some(parse_quality(&release.title)),
            quality_score: None,
            quality_rejected: false,
            quality_reject_reasons: vec![],
            quality_breakdown: None,
            match_explanation: None,
            structural_rejected: false,
            structural_reject_reason: None,
            total_score: 0.0,
            episode_key: None,
            is_preferred_protocol: false,
            structural_bonus: 0.0,
            seeder_bonus: 0.0,
            freshness_bonus: 0.0,
            context_flags: vec![],
        };
    }

    // ── Gate 2: Category (hard early return) ──
    if let Some(allowed_cats) = config.allowed_categories
        && !crate::helpers::release_matches_categories(&release.category_ids, allowed_cats)
    {
        return ProcessedRelease {
            release: release.clone().into(),
            decision: ScoringDecision::Rejected,
            reject_reasons: vec![RejectReason::CategoryMismatch],
            match_score: None,
            matched_pattern: None,
            matched_target: None,
            parsed_quality: Some(parse_quality(&release.title)),
            quality_score: None,
            quality_rejected: false,
            quality_reject_reasons: vec![],
            quality_breakdown: None,
            match_explanation: None,
            structural_rejected: false,
            structural_reject_reason: None,
            total_score: 0.0,
            episode_key: None,
            is_preferred_protocol: false,
            structural_bonus: 0.0,
            seeder_bonus: 0.0,
            freshness_bonus: 0.0,
            context_flags: vec![],
        };
    }

    // ── All remaining gates run unconditionally ──
    let mut reject_reasons: Vec<RejectReason> = Vec::new();

    // Parse-once optimization: structural_reject and structural_bonus both call
    // parse_release internally. For everything except sports wants, both sides
    // use the same media_kind — parse once here and reuse. Sports falls back to
    // the non-parsed variants (reject uses "sports", bonus uses "tv").
    let prep: Option<ParsedRelease> = config.media_want.and_then(|mw| {
        let (r, b) = structural_media_kinds(mw);
        (r == b).then(|| parse_release(&release.title, r))
    });

    // Gate 3: Structural rejection
    let structural_result = match &prep {
        Some(parsed) => structural_reject_with_parsed(
            release,
            parsed,
            config.media_want,
            config.search_context,
            config.tv_naming(),
        ),
        None => structural_reject(release, config.media_want, config.search_context),
    };
    let structural_rejected = structural_result.is_some();
    if let Some(reason) = structural_result {
        reject_reasons.push(RejectReason::Structural(reason));
    }

    // Gate 4: Votes rejection
    if votes_reject(release).is_some() {
        reject_reasons.push(RejectReason::NegativeVotes);
    }

    // Gate 5: Pattern/target matching (always run)
    let (match_score, matched_pattern, matched_target) = match match_gate(
        release,
        &PreparedTitle::new(&release.title),
        config,
        config
            .media_want
            .and_then(sport::SportVocab::from_want)
            .map(|v| {
                sport::judge(
                    &v,
                    &sport::tokenize(&release.title),
                    release.size,
                    parse_quality(&release.title).resolution.as_deref(),
                    sport::publish_date(&release.publish_date),
                )
            })
            .as_ref(),
    ) {
        Some((score, pattern, target)) => (Some(score), Some(pattern), target),
        None => (None, None, None),
    };

    let is_no_match = match_score.is_none();

    // Gate 6: Quality scoring (always run)
    let quality = score_quality(
        &release.title,
        config.quality_profile,
        config.quality_overrides,
        config.trash_filters,
    );
    let quality_rejected = quality.rejected;
    let quality_reject_reasons = quality.reject_reasons.clone();
    if quality_rejected {
        for qr in &quality.reject_reasons {
            reject_reasons.push(RejectReason::Quality(*qr));
        }
    }

    // Gate 7: Episode filter (TvSeason only)
    let episode_key = if let Some(episodes_needed) = config.episodes_needed
        && let Some(mw_json) = config.media_want_json
    {
        let key = crate::watch::episode_key::extract_episode_key(&release.title, mw_json);
        if let Some(ref k) = key
            && !crate::watch::episode_key::is_episode_needed(k, episodes_needed)
        {
            reject_reasons.push(RejectReason::EpisodeNotNeeded);
        }
        key
    } else {
        None
    };

    // ── Bonuses ──
    let seeder_bonus = seeder_tiebreaker(release);
    let struct_bonus = match &prep {
        Some(parsed) => {
            structural_bonus_with_parsed(release, parsed, config.media_want, config.xem_show_map)
        }
        None => structural_bonus(release, config.media_want, config.xem_show_map),
    };
    let fresh_bonus = freshness_tiebreaker(release);

    let total_score =
        match_score.unwrap_or(0.0) + quality.score + seeder_bonus + struct_bonus + fresh_bonus;

    // Protocol preference
    let is_preferred_protocol = config
        .preferred_protocol
        .is_some_and(|pref| release.protocol == pref);

    // ── Final decision ──
    let decision = if !reject_reasons.is_empty() {
        if is_no_match
            && reject_reasons.iter().all(|r| {
                !matches!(
                    r,
                    RejectReason::Quality(_)
                        | RejectReason::Structural(_)
                        | RejectReason::NegativeVotes
                        | RejectReason::CategoryMismatch
                        | RejectReason::EpisodeNotNeeded
                        | RejectReason::Context(_)
                )
            })
        {
            ScoringDecision::NoMatch
        } else {
            ScoringDecision::Rejected
        }
    } else if is_no_match {
        ScoringDecision::NoMatch
    } else {
        ScoringDecision::Candidate
    };

    ProcessedRelease {
        release: release.clone().into(),
        decision,
        reject_reasons,
        match_score,
        matched_pattern,
        matched_target,
        // score_quality already parsed the title; reuse it
        parsed_quality: Some(quality.parsed),
        quality_score: Some(quality.score),
        quality_rejected,
        quality_reject_reasons,
        quality_breakdown: None,
        match_explanation: None,
        structural_rejected,
        structural_reject_reason: structural_result,
        total_score,
        episode_key,
        is_preferred_protocol,
        structural_bonus: struct_bonus,
        seeder_bonus,
        freshness_bonus: fresh_bonus,
        context_flags: vec![],
    }
}

#[test]
fn shared_release_facts_score_like_the_reference() {
    use crate::services::want::release_facts::ReleaseFacts;
    use crate::watch::service::{build_scoring_config, has_match_config};
    use rayon::prelude::*;

    let Some(dir) = std::env::var_os("KOBLOAD_SCORING_CORPUS").map(std::path::PathBuf::from) else {
        eprintln!("KOBLOAD_SCORING_CORPUS not set; skipping");
        return;
    };
    let step = std::env::var("KOBLOAD_SCORING_STEP")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);
    let corpus: Vec<_> = crate::watch::capture::read_capture(&dir)
        .expect("read capture directory")
        .into_iter()
        .step_by(step)
        .collect();

    // Every matched pair, plus every 97th release against every pattern so the
    // no-match paths are compared too.
    let compared: usize = corpus
        .par_iter()
        .map(|f| {
            crate::clock::with_pinned_now(f.fetch.captured_at, || {
                let facts: Vec<_> = f.fetch.releases.iter().map(ReleaseFacts::new).collect();
                let mut compared = 0;
                for prep in f.prepared.iter().filter(|p| has_match_config(p)) {
                    let config = build_scoring_config(prep, f.trash.as_deref());
                    for (i, (release, facts)) in f.fetch.releases.iter().zip(&facts).enumerate() {
                        if i % 97 != 0 && !release_matches(release, &config) {
                            continue;
                        }
                        let old = serde_json::to_value(score_release(release, &config)).unwrap();
                        let new =
                            serde_json::to_value(score_release_facts(facts, &config)).unwrap();
                        assert_eq!(old, new, "{}", release.title);
                        compared += 1;
                    }
                }
                compared
            })
        })
        .sum();
    eprintln!(
        "{compared} pairs over {} fetches score the same",
        corpus.len()
    );
    assert!(compared > 0);
}
