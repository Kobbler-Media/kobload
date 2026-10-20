use std::collections::{HashMap, HashSet};

use sqlx::PgPool;
use time::OffsetDateTime;
use tracing::warn;
use uuid::Uuid;

use super::binge;
use super::outcomes::{self, TraitRisk};
use crate::repositories::stream_outcomes as repo;
use nats_jobs::types::{StreamOption, StreamPlayback, StreamPreference};

/// Known availability of a hash on a debrid provider.
///
/// Ordered best-first; the discriminant is the ranking tier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum KnownAvailability {
    /// We resolved this ourselves, recently. The strongest signal there is.
    Available,
    /// Torrentio reports it cached. Weaker than our own probe: the status comes
    /// from Torrentio's shared store (~5-day TTL, populated when *any* of its
    /// users unrestricted the hash), not from a check against our account, so
    /// it means "recently cached for someone" rather than "cached for us".
    LikelyAvailable,
    /// Nothing known either way.
    Unknown,
    /// Torrentio reports it not cached — it would have to download first.
    LikelyDead,
    /// Our own probe failed to cache it (no seeds / timed out). Deprioritise,
    /// but still offer it if nothing better exists — it may have recovered.
    Dead,
    /// Provider refuses to serve it (RD filter-gate). Never offer it.
    Blocked,
}

pub struct RankProvider {
    pub id: Uuid,
    pub provider_type: String,
}

pub struct RankCandidate {
    pub option: StreamOption,
    pub availability: KnownAvailability,
}

type ReleaseKey = (String, Option<i32>);

/// Clients resolve the first listed file, so that is the file an option stands for.
fn release_key(option: &StreamOption) -> Option<ReleaseKey> {
    Some((
        option.info_hash.as_deref()?.to_lowercase(),
        option.files.first().map(|f| f.id),
    ))
}

struct Continuity {
    info_hash: String,
    binge_group: Option<String>,
    release_group: Option<String>,
}

impl Continuity {
    fn rank(&self, option: &StreamOption, release_group: Option<&str>) -> u8 {
        if option
            .info_hash
            .as_deref()
            .is_some_and(|h| h.eq_ignore_ascii_case(&self.info_hash))
        {
            0
        } else if self.binge_group.is_some() && option.binge_group == self.binge_group {
            1
        } else if self.release_group.is_some() && release_group == self.release_group.as_deref() {
            2
        } else {
            3
        }
    }
}

fn release_group(title: &str) -> Option<String> {
    crate::quality::parse_quality(title)
        .release_group
        .map(|g| g.to_lowercase())
}

/// Order is: a release that just failed for a client sinks below everything; then
/// availability tier; then continuity with what the caller played before; then learned
/// name risk; then the incoming (score) order, which the stable sort preserves.
/// Availability is a hard tier rather than a score bonus: score ranges shift per quality
/// profile, so any constant would be either too small to matter or swamp quality.
///
/// Warming walks this order, so it must be final — callers must not reorder it.
pub async fn rank_options(
    pool: &PgPool,
    provider: &RankProvider,
    candidates: Vec<RankCandidate>,
    prefer: Option<&StreamPreference>,
) -> Vec<StreamOption> {
    for c in &candidates {
        if let (Some(hash), Some(group)) = (&c.option.info_hash, &c.option.binge_group) {
            binge::remember(hash, group);
        }
    }

    let mut hashes: HashSet<String> = candidates
        .iter()
        .filter_map(|c| c.option.info_hash.as_deref().map(str::to_lowercase))
        .collect();
    if let Some(p) = prefer {
        hashes.insert(p.info_hash.to_lowercase());
    }
    let hashes: Vec<String> = hashes.into_iter().collect();
    let rows = repo::releases_for(pool, provider.id, &hashes)
        .await
        .unwrap_or_else(|e| {
            warn!(error = %e, "release outcome lookup failed; ranking without history");
            Vec::new()
        });

    let now = OffsetDateTime::now_utc();
    let verdicts: HashMap<ReleaseKey, StreamPlayback> = rows
        .iter()
        .filter_map(|r| {
            outcomes::verdict(r.last_success_at, r.last_failure_at, now)
                .map(|v| ((r.info_hash.clone(), r.file_id), v))
        })
        .collect();

    let continuity = prefer.map(|p| {
        let hash = p.info_hash.to_lowercase();
        Continuity {
            binge_group: rows
                .iter()
                .find(|r| r.info_hash == hash && r.binge_group.is_some())
                .and_then(|r| r.binge_group.clone())
                .or_else(|| binge::lookup(&hash)),
            release_group: p.release_title.as_deref().and_then(release_group),
            info_hash: hash,
        }
    });

    let traits: Vec<Vec<String>> = candidates
        .iter()
        .map(|c| outcomes::release_traits(&c.option.release_title))
        .collect();
    let mut all_traits: Vec<String> = traits.iter().flatten().cloned().collect();
    all_traits.sort();
    all_traits.dedup();
    let risk = TraitRisk::load(pool, provider.id, &provider.provider_type, &all_traits).await;

    let mut keyed: Vec<((bool, KnownAvailability, u8, u8), StreamOption)> = candidates
        .into_iter()
        .zip(traits)
        .map(|(c, traits)| {
            let mut option = c.option;
            option.playback = release_key(&option).and_then(|k| verdicts.get(&k).copied());
            let continuity_rank = continuity.as_ref().map_or(3, |cont| {
                cont.rank(&option, release_group(&option.release_title).as_deref())
            });
            let key = (
                option.playback == Some(StreamPlayback::Failed),
                c.availability,
                continuity_rank,
                risk.bucket(&traits),
            );
            (key, option)
        })
        .collect();
    keyed.sort_by_key(|(key, _)| *key);
    keyed.into_iter().map(|(_, o)| o).collect()
}
