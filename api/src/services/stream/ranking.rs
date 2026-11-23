use std::collections::{HashMap, HashSet};

use sqlx::PgPool;
use time::OffsetDateTime;
use tracing::warn;
use uuid::Uuid;

use super::binge;
use super::continuity::{self, Continuity};
use super::outcomes;
use super::rank_key::{RankFacts, RankKey};
use super::release_fit;
use super::trait_risk::{self, TraitRisk};
use crate::language::{self, LanguageFit};
use crate::repositories::stream_outcomes as repo;
use nats_jobs::types::{
    StreamAvailability, StreamGroup, StreamOption, StreamPlayback, StreamPreference,
    StreamSourceType,
};

/// Known availability of a hash on a debrid provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KnownAvailability {
    /// A finished copy is on the account already; playing it adds nothing.
    InAccount,
    /// We resolved this ourselves, recently. The strongest signal there is.
    Available,
    /// Torrentio reports it cached. Weaker than our own probe: the status comes
    /// from Torrentio's shared store (14-day TTL, populated when *any* of its
    /// users unrestricted the hash), not from a check against our account, so
    /// it means "recently cached for someone" rather than "cached for us".
    LikelyAvailable,
    /// Nothing known either way.
    Unknown,
    /// Its name carries a tag the provider refuses, or Torrentio left it out of its debrid
    /// listing, which it does for hashes any of its users had refused as infringing.
    LikelyRefused,
    /// Torrentio reports it not cached — it would have to download first.
    LikelyDead,
    /// Our own probe failed to cache it (no seeds / timed out). Deprioritise,
    /// but still offer it if nothing better exists — it may have recovered.
    Dead,
    /// Provider refused to add it (RD filter-gate). RD does not refuse a hash every time,
    /// so it is offered last rather than hidden.
    Blocked,
}

pub struct RankProvider {
    pub id: Uuid,
}

pub struct RankCandidate {
    pub option: StreamOption,
    pub availability: KnownAvailability,
}

type OutcomeKey = (String, Option<i32>);

/// Clients resolve the first listed file, so that is the file an option stands for.
fn outcome_key(option: &StreamOption) -> Option<OutcomeKey> {
    Some((
        option.release_key.as_deref()?.to_lowercase(),
        option.files.first().map(|f| f.id),
    ))
}

/// A usenet release's files are listed only once it is imported, so the play before that
/// is reported for the release as a whole.
fn release_wide(option: &StreamOption, key: &OutcomeKey) -> Option<OutcomeKey> {
    (option.source_type == StreamSourceType::Usenet && key.1.is_some())
        .then(|| (key.0.clone(), None))
}

/// A release whose default audio track is likely another language than the preferred
/// one still wins against one this much higher in score.
const LANGUAGE_ORDER_MARGIN: f64 = 0.1;

#[derive(Default)]
pub struct RankPreferences<'a> {
    pub prefer: Option<&'a StreamPreference>,
    pub audio_languages: &'a [String],
}

/// A release that played starts again; its availability only matters once a probe says
/// it is gone. Copies on the account share a section with the hints they compete with on
/// score, and whatever already failed for us is at the bottom.
fn group(availability: KnownAvailability, played: bool, playback_failed: bool) -> StreamGroup {
    use KnownAvailability as A;
    match availability {
        _ if playback_failed => StreamGroup::Failed,
        A::Dead | A::Blocked => StreamGroup::Failed,
        A::LikelyDead | A::LikelyRefused => StreamGroup::Uncached,
        _ if played => StreamGroup::Played,
        A::InAccount | A::Available | A::LikelyAvailable | A::Unknown => StreamGroup::Available,
    }
}

fn exposed(availability: KnownAvailability, played: bool) -> StreamAvailability {
    use KnownAvailability as A;
    match availability {
        A::Available | A::InAccount => StreamAvailability::Ready,
        A::LikelyAvailable => StreamAvailability::Likely,
        A::Unknown if played => StreamAvailability::Likely,
        A::Unknown => StreamAvailability::Unknown,
        A::LikelyDead | A::LikelyRefused | A::Dead | A::Blocked => StreamAvailability::Uncached,
    }
}

/// Order is: releases that can start now; then hardcoded subtitles sink, ahead of the group,
/// as a picture with burned-in subtitles is no use however readily it plays; then the
/// group. Among available releases, cached ones (on the account, probed ready, RD+ or our
/// usenet engine) rank above unvouched ones and compete on score, with a small edge for a
/// copy certain to start; see `RankKey` for how language, fakes, continuity and learned risk
/// fit in. Score is discounted for releases many devices cannot play, whose default audio
/// track is not the preferred language, or that look different from the previous episode.
/// The pack the previous episode played from ranks as played: the next episode in it was
/// selected along with that one.
/// The group is a hard tier rather than a score bonus: score ranges shift per quality
/// profile, so any constant would be either too small to matter or swamp quality.
///
/// Warming walks this order, so it must be final — callers must not reorder it.
pub async fn rank_options(
    pool: &PgPool,
    provider: &RankProvider,
    candidates: Vec<RankCandidate>,
    preferences: &RankPreferences<'_>,
) -> Vec<StreamOption> {
    let prefer = preferences.prefer;
    let audio_languages = language::normalise(preferences.audio_languages);
    for c in &candidates {
        if let (Some(key), Some(group)) = (&c.option.release_key, &c.option.binge_group) {
            binge::remember(key, group);
        }
    }

    let mut keys: HashSet<String> = candidates
        .iter()
        .filter_map(|c| c.option.release_key.as_deref().map(str::to_lowercase))
        .collect();
    if let Some(p) = prefer {
        keys.insert(p.release_key.to_lowercase());
    }
    let keys: Vec<String> = keys.into_iter().collect();
    let mut provider_ids: Vec<Uuid> = candidates
        .iter()
        .map(|c| c.option.debrid_provider_id)
        .chain([provider.id])
        .collect();
    provider_ids.sort_unstable();
    provider_ids.dedup();
    let rows = repo::releases_for(pool, &provider_ids, &keys)
        .await
        .unwrap_or_else(|e| {
            warn!(error = %e, "release outcome lookup failed; ranking without history");
            Vec::new()
        });

    let now = OffsetDateTime::now_utc();
    let verdicts: HashMap<OutcomeKey, StreamPlayback> = rows
        .iter()
        .filter_map(|r| {
            let verdict = match r.reported_bad_at {
                Some(_) => Some(StreamPlayback::Failed),
                None => outcomes::verdict(r.last_success_at, r.last_failure_at, now),
            };
            verdict.map(|v| ((r.release_key.clone(), r.file_id), v))
        })
        .collect();
    let reported: HashSet<OutcomeKey> = rows
        .iter()
        .filter(|r| r.reported_bad_at.is_some())
        .map(|r| (r.release_key.clone(), r.file_id))
        .collect();

    let continuity = prefer.map(|p| {
        let key = p.release_key.to_lowercase();
        let binge_group = rows
            .iter()
            .find(|r| r.release_key == key && r.binge_group.is_some())
            .and_then(|r| r.binge_group.clone())
            .or_else(|| binge::lookup(&key));
        Continuity::new(key, binge_group, p.release_title.as_deref())
    });
    let previous_failed = prefer.is_some_and(|p| {
        verdicts.get(&(p.release_key.to_lowercase(), p.file_id)) == Some(&StreamPlayback::Failed)
    });

    let traits: Vec<Vec<String>> = candidates
        .iter()
        .map(|c| trait_risk::release_traits(&c.option.release_title))
        .collect();
    let mut all_traits: Vec<String> = traits.iter().flatten().cloned().collect();
    all_traits.sort();
    all_traits.dedup();
    let risk = TraitRisk::load(pool, provider.id, &all_traits).await;

    let mut keyed: Vec<(RankKey, f64, StreamOption)> = candidates
        .into_iter()
        .zip(traits)
        .map(|(c, traits)| {
            let mut option = c.option;
            let key = outcome_key(&option);
            option.playback = key.as_ref().and_then(|k| {
                verdicts
                    .get(k)
                    .or_else(|| release_wide(&option, k).and_then(|k| verdicts.get(&k)))
                    .copied()
            });
            option.reported_bad = key.is_some_and(|k| reported.contains(&k));
            let quality = crate::quality::parse_quality(&option.release_title);
            let continuity_rank = continuity
                .as_ref()
                .map_or(continuity::Rank::None, |cont| cont.rank(&option, &quality));
            let same_pack = continuity_rank == continuity::Rank::SamePack
                && !previous_failed
                && !matches!(
                    c.availability,
                    KnownAvailability::Dead | KnownAvailability::Blocked
                );
            let played = option.playback == Some(StreamPlayback::Worked) || same_pack;
            let availability = if same_pack {
                KnownAvailability::Available
            } else {
                c.availability
            };
            let languages = language::parse_audio(&option.release_title);
            let language_fit = language::fit(&languages, &audio_languages);
            option.audio_languages = languages.listed.iter().map(|l| l.to_string()).collect();
            let size = option
                .files
                .first()
                .map(|f| f.size)
                .filter(|s| *s > 0)
                .unwrap_or(option.size);
            let fit =
                release_fit::assess(&option.release_title, &quality, size, option.total_score);
            let risk = risk.risk(&traits);
            let exposed = exposed(availability, played);
            option.cached = exposed == StreamAvailability::Ready;
            option.availability = Some(exposed);
            let mut penalty = continuity
                .as_ref()
                .map_or(0.0, |cont| cont.style_penalty(&option, &quality));
            if language_fit == LanguageFit::Secondary {
                penalty += LANGUAGE_ORDER_MARGIN;
            }
            let group = group(
                availability,
                played,
                option.playback == Some(StreamPlayback::Failed),
            );
            option.group = Some(group);
            let facts = RankFacts {
                group,
                hardcoded: quality.is_hardcoded_subs,
                language_missing: language_fit == LanguageFit::Missing,
                suspect: fit.suspect,
                continuity: continuity_rank,
                risk: risk.bucket(),
                ready: matches!(
                    availability,
                    KnownAvailability::InAccount | KnownAvailability::Available
                ),
                likely: availability == KnownAvailability::LikelyAvailable,
            };
            let score = fit.sort_score - penalty * fit.sort_score.abs() + facts.score_edge();
            (facts.key(), score, option)
        })
        .collect();
    keyed.sort_by(|(ka, sa, _), (kb, sb, _)| ka.cmp(kb).then(sb.total_cmp(sa)));
    keyed.into_iter().map(|(_, _, o)| o).collect()
}
