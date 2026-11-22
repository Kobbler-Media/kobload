use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use dashmap::DashMap;
use tracing::debug;
use usenet_proto::OutputInfo;

use crate::debrid::DebridProvider;

const TTL: Duration = Duration::from_secs(30 * 60);
/// A search lists files only when the engine answers at once; it never waits on one.
const LOOKUP_WAIT: Duration = Duration::from_secs(2);
const MAX_ENTRIES: usize = 2_000;

type Listed = (Arc<Vec<OutputInfo>>, Instant);

static OUTPUTS: LazyLock<DashMap<String, Listed>> = LazyLock::new(DashMap::new);

pub fn store(release_key: &str, outputs: Vec<OutputInfo>) {
    if OUTPUTS.len() > MAX_ENTRIES {
        OUTPUTS.retain(|_, (_, at)| at.elapsed() < TTL);
    }
    OUTPUTS.insert(release_key.to_string(), (Arc::new(outputs), Instant::now()));
}

pub async fn outputs(
    provider: &dyn DebridProvider,
    release_key: &str,
) -> Option<Arc<Vec<OutputInfo>>> {
    if let Some(entry) = OUTPUTS.get(release_key)
        && entry.1.elapsed() < TTL
    {
        return Some(entry.0.clone());
    }
    match tokio::time::timeout(LOOKUP_WAIT, provider.usenet_release(release_key)).await {
        Ok(Ok(release)) => {
            store(release_key, release.outputs);
            OUTPUTS.get(release_key).map(|e| e.0.clone())
        }
        Ok(Err(e)) => {
            debug!(release_key, error = %e, "Usenet engine did not list the release's files");
            None
        }
        Err(_) => None,
    }
}
