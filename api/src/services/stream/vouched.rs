use std::sync::LazyLock;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use uuid::Uuid;

/// Releases Torrentio's search showed as `[RD+]`: someone on Real-Debrid streamed them
/// recently, whatever the name rule says. Kept only from the search to the play it offers.
static VOUCHED: LazyLock<DashMap<(Uuid, String), Instant>> = LazyLock::new(DashMap::new);

const TTL: Duration = Duration::from_secs(6 * 3600);
const MAX_ENTRIES: usize = 20_000;

pub fn remember(provider_id: Uuid, info_hash: &str) {
    VOUCHED.insert((provider_id, info_hash.to_lowercase()), Instant::now());
    if VOUCHED.len() > MAX_ENTRIES {
        VOUCHED.retain(|_, at| at.elapsed() < TTL);
    }
}

pub fn is_vouched(provider_id: Uuid, info_hash: &str) -> bool {
    VOUCHED
        .get(&(provider_id, info_hash.to_lowercase()))
        .is_some_and(|at| at.elapsed() < TTL)
}
