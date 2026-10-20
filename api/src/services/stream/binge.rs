use std::sync::LazyLock;
use std::time::{Duration, Instant};

use dashmap::DashMap;

/// Torrentio hands out binge groups only inside search responses, so the group of the
/// stream a client is now playing is known only from the search that offered it.
static BINGE_GROUPS: LazyLock<DashMap<String, (String, Instant)>> = LazyLock::new(DashMap::new);

const TTL: Duration = Duration::from_secs(7 * 86_400);
const MAX_ENTRIES: usize = 20_000;

pub fn remember(info_hash: &str, binge_group: &str) {
    BINGE_GROUPS.insert(
        info_hash.to_lowercase(),
        (binge_group.to_string(), Instant::now()),
    );
    if BINGE_GROUPS.len() > MAX_ENTRIES {
        BINGE_GROUPS.retain(|_, (_, at)| at.elapsed() < TTL);
    }
}

pub fn lookup(info_hash: &str) -> Option<String> {
    let entry = BINGE_GROUPS.get(&info_hash.to_lowercase())?;
    (entry.1.elapsed() < TTL).then(|| entry.0.clone())
}
