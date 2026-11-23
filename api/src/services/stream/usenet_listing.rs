use std::collections::HashMap;

use sqlx::PgPool;
use tracing::warn;
use uuid::Uuid;

use super::availability::known_availability;
use super::ranking::{KnownAvailability, RankCandidate};
use super::usenet_files;
use crate::debrid::{DebridProviderResolver, DebridProviderType};
use crate::repositories::usenet_releases::{self, Listing};
use crate::usenet::outputs::{Target, choose, listed_files};
use crate::watch::{ProcessedRelease, Release};
use nats_jobs::types::{StreamOption, StreamSourceType};

pub struct UsenetProviderRow {
    pub id: Uuid,
    pub provider_type: String,
}

pub fn listing(release: &Release, download_url: &str) -> Listing {
    Listing {
        indexer_id: release
            .indexer_source_id
            .map(|id| id.to_string())
            .unwrap_or_else(|| release.indexer.clone()),
        guid: release.guid.clone(),
        download_url: download_url.to_string(),
        release_title: release.title.clone(),
    }
}

pub fn options(
    result: &ProcessedRelease,
    listing: &Listing,
    providers: &[UsenetProviderRow],
) -> Vec<RankCandidate> {
    let nzb_ref = listing.nzb_ref();
    providers
        .iter()
        .map(|provider| RankCandidate {
            option: StreamOption {
                id: format!("usenet_{}_{}", nzb_ref.guid, provider.id),
                release_title: result.release.title.clone(),
                size: result.release.size,
                indexer: result.release.indexer.clone(),
                release_key: None,
                nzb_ref: Some(nzb_ref.clone()),
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

fn on_engine(candidate: &RankCandidate) -> bool {
    DebridProviderType::parse(&candidate.option.debrid_provider) == Some(DebridProviderType::Usenet)
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

    let engine_configured = resolver.usenet_engine().is_some();
    let mut engine_keys = Vec::new();
    for c in candidates
        .iter_mut()
        .filter(|c| c.option.source_type == StreamSourceType::Usenet)
    {
        if let Some(key) = &c.option.release_key
            && c.availability == KnownAvailability::Unknown
            && let Some(availability) = known.get(&(c.option.debrid_provider_id, key.clone()))
        {
            c.availability = *availability;
        }
        // Our engine has no cached-check, but posts within retention nearly always stream,
        // and a release that failed is already marked Dead above. Without this hint kobbler
        // would never warm a usenet pick at all.
        if on_engine(c) && engine_configured && c.availability == KnownAvailability::Unknown {
            c.availability = KnownAvailability::LikelyAvailable;
        }
        if on_engine(c)
            && let Some(key) = &c.option.release_key
        {
            engine_keys.push(key.clone());
        }
    }
    if engine_keys.is_empty() {
        return;
    }
    let engine = resolver.get(&DebridProviderType::Usenet);
    let files = usenet_files::lookup(engine.as_ref(), engine_keys).await;
    for c in candidates.iter_mut().filter(|c| on_engine(c)) {
        if let Some(outputs) = c.option.release_key.as_ref().and_then(|k| files.get(k)) {
            c.option.files = listed_files(outputs, wanted);
            // Judged per search from the file list, so a pack missing this episode stays
            // the pick for the episodes it does hold.
            if choose(outputs, None, wanted).is_err() {
                c.availability = KnownAvailability::Dead;
            }
        }
    }
}
