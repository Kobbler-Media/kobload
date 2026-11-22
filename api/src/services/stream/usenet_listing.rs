use sqlx::PgPool;
use tracing::warn;

use super::ranking::RankCandidate;
use crate::repositories::usenet_releases::{self, Listing};
use crate::watch::Release;
use nats_jobs::NzbRef;
use nats_jobs::types::StreamSourceType;

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

/// An option resolves only through its stored listing, so one that could not be stored is
/// dropped rather than offered.
pub async fn remember(pool: &PgPool, candidates: &mut Vec<RankCandidate>, listings: &[Listing]) {
    match usenet_releases::upsert(pool, listings).await {
        Ok(keys) => {
            for c in candidates.iter_mut() {
                if let Some(nzb_ref) = &c.option.nzb_ref {
                    c.option.release_key = keys.get(nzb_ref).cloned();
                }
            }
        }
        Err(e) => {
            warn!(error = %e, count = listings.len(), "Failed to store usenet listings");
            candidates.retain(|c| c.option.source_type != StreamSourceType::Usenet);
        }
    }
}
