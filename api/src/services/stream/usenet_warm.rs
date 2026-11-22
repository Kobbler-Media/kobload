use std::sync::LazyLock;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use uuid::Uuid;

use nats_jobs::NzbRef;
use nats_jobs::types::{StreamOption, StreamSourceType};

/// Each warm import of an NZB we do not hold yet costs an indexer grab, and a prewarm burst
/// walks about thirteen releases. So only the option a search would auto-play is imported
/// ahead of a tap; the rest of the walk waits for one.
const TTL: Duration = Duration::from_secs(30 * 60);
const MAX_ENTRIES: usize = 2_000;

static AUTO_PLAY: LazyLock<DashMap<(Uuid, NzbRef), Instant>> = LazyLock::new(DashMap::new);

pub fn remember_auto_play(options: &[StreamOption]) {
    let Some(first) = options.first() else {
        return;
    };
    if first.source_type != StreamSourceType::Usenet {
        return;
    }
    let Some(nzb_ref) = &first.nzb_ref else {
        return;
    };
    if AUTO_PLAY.len() > MAX_ENTRIES {
        AUTO_PLAY.retain(|_, at| at.elapsed() < TTL);
    }
    AUTO_PLAY.insert((first.debrid_provider_id, nzb_ref.clone()), Instant::now());
}

pub fn may_warm(provider_id: Uuid, nzb_ref: &NzbRef) -> bool {
    AUTO_PLAY
        .get(&(provider_id, nzb_ref.clone()))
        .is_some_and(|at| at.elapsed() < TTL)
}
