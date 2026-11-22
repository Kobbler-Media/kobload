use std::collections::HashMap;
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use sqlx::PgPool;
use tracing::warn;
use uuid::Uuid;

use super::availability::known_availability;
use super::ranking::{KnownAvailability, RankCandidate};
use super::usenet_files;
use crate::debrid::{DebridProviderResolver, DebridProviderType};
use crate::repositories::usenet_releases::{self, Listing};
use crate::usenet::outputs::{Target, listed_files};
use crate::watch::{ProcessedRelease, Release};
use nats_jobs::NzbRef;
use nats_jobs::types::{StreamOption, StreamSourceType};

/// A pack is listed for one episode but imported as a whole, so the episode a search wanted
/// is kept until the resolve that follows it.
const TARGET_TTL: Duration = Duration::from_secs(60 * 60);
const MAX_TARGETS: usize = 5_000;

static TARGETS: LazyLock<DashMap<NzbRef, (Target, Instant)>> = LazyLock::new(DashMap::new);

pub fn target(nzb_ref: &NzbRef) -> Option<Target> {
    TARGETS
        .get(nzb_ref)
        .filter(|e| e.1.elapsed() < TARGET_TTL)
        .map(|e| e.0.clone())
}

pub struct UsenetProviderRow {
    pub id: Uuid,
    pub provider_type: String,
}

pub fn listing(release: &Release, download_url: &str) -> Listing {
    Listing {
        nzb_ref: NzbRef {
            indexer_id: release
                .indexer_source_id
                .map(|id| id.to_string())
                .unwrap_or_else(|| release.indexer.clone()),
            guid: release.guid.clone(),
        },
        download_url: download_url.to_string(),
        release_title: release.title.clone(),
    }
}

pub fn options(
    result: &ProcessedRelease,
    listing: &Listing,
    providers: &[UsenetProviderRow],
) -> Vec<RankCandidate> {
    providers
        .iter()
        .map(|provider| RankCandidate {
            option: StreamOption {
                id: format!("usenet_{}_{}", result.release.guid, provider.id),
                release_title: result.release.title.clone(),
                size: result.release.size,
                indexer: result.release.indexer.clone(),
                release_key: None,
                nzb_ref: Some(listing.nzb_ref.clone()),
                magnet_url: None,
                quality: result
                    .parsed_quality
                    .as_ref()
                    .and_then(|pq| serde_json::to_value(pq).ok()),
                total_score: result.total_score,
                match_score: result.match_score.unwrap_or(0.0),
                quality_score: result.quality_score.unwrap_or(0.0),
                debrid_provider: provider.provider_type.clone(),
                debrid_provider_id: provider.id,
                cached: false,
                source_type: StreamSourceType::Usenet,
                files: Vec::new(),
                publish_date: Some(result.release.publish_date.clone()),
                binge_group: None,
                playback: None,
                reported_bad: false,
                availability: None,
                group: None,
                audio_languages: Vec::new(),
            },
            availability: KnownAvailability::Unknown,
        })
        .collect()
}

/// An option resolves only through its stored listing, so one that could not be stored is
/// dropped rather than offered. An imported one carries its release key, what we know of
/// it, and on our own engine the files it holds.
pub async fn remember(
    pool: &PgPool,
    resolver: &dyn DebridProviderResolver,
    candidates: &mut Vec<RankCandidate>,
    listings: &[Listing],
    wanted: &Target,
) {
    let listed = match usenet_releases::upsert(pool, listings).await {
        Ok(listed) => listed,
        Err(e) => {
            warn!(error = %e, count = listings.len(), "Failed to store usenet listings");
            candidates.retain(|c| c.option.source_type != StreamSourceType::Usenet);
            return;
        }
    };
    if TARGETS.len() > MAX_TARGETS {
        TARGETS.retain(|_, (_, at)| at.elapsed() < TARGET_TTL);
    }
    for listing in listings {
        TARGETS.insert(listing.nzb_ref.clone(), (wanted.clone(), Instant::now()));
    }

    let mut keys_by_provider: HashMap<Uuid, Vec<String>> = HashMap::new();
    for c in candidates.iter_mut() {
        let Some(state) = c.option.nzb_ref.as_ref().and_then(|r| listed.get(r)) else {
            continue;
        };
        c.option.release_key = state.release_key.clone();
        if state.failed_recently {
            c.availability = KnownAvailability::Dead;
        }
        if let Some(key) = &state.release_key {
            keys_by_provider
                .entry(c.option.debrid_provider_id)
                .or_default()
                .push(key.clone());
        }
    }
    let mut known: HashMap<(Uuid, String), KnownAvailability> = HashMap::new();
    for (provider_id, keys) in keys_by_provider {
        for (key, availability) in known_availability(pool, provider_id, &keys).await {
            known.insert((provider_id, key), availability);
        }
    }

    let engine = resolver.get(&DebridProviderType::Usenet);
    for c in candidates
        .iter_mut()
        .filter(|c| c.option.source_type == StreamSourceType::Usenet)
    {
        let Some(key) = c.option.release_key.clone() else {
            continue;
        };
        if c.availability == KnownAvailability::Unknown
            && let Some(availability) = known.get(&(c.option.debrid_provider_id, key.clone()))
        {
            c.availability = *availability;
        }
        if DebridProviderType::parse(&c.option.debrid_provider) == Some(DebridProviderType::Usenet)
            && let Some(outputs) = usenet_files::outputs(engine.as_ref(), &key).await
        {
            c.option.files = listed_files(&outputs, wanted);
        }
    }
}
