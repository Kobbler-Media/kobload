use std::collections::HashMap;
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use dashmap::DashMap;
use futures::StreamExt;
use futures::stream::FuturesUnordered;
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

fn cached(release_key: &str) -> Option<Arc<Vec<OutputInfo>>> {
    OUTPUTS
        .get(release_key)
        .filter(|entry| entry.1.elapsed() < TTL)
        .map(|entry| entry.0.clone())
}

pub async fn lookup(
    provider: &dyn DebridProvider,
    release_keys: Vec<String>,
) -> HashMap<String, Arc<Vec<OutputInfo>>> {
    let mut found = HashMap::new();
    let mut pending = FuturesUnordered::new();
    for key in release_keys {
        if found.contains_key(&key) {
            continue;
        }
        match cached(&key) {
            Some(outputs) => {
                found.insert(key, outputs);
            }
            None => pending.push(async move {
                let release = provider.usenet_release(&key).await;
                (key, release)
            }),
        }
    }
    let deadline = tokio::time::Instant::now() + LOOKUP_WAIT;
    while let Ok(Some((key, release))) = tokio::time::timeout_at(deadline, pending.next()).await {
        match release {
            Ok(release) => {
                store(&key, release.outputs);
                if let Some(outputs) = cached(&key) {
                    found.insert(key, outputs);
                }
            }
            Err(e) => {
                debug!(release_key = key, error = %e, "Usenet engine did not list the release's files");
            }
        }
    }
    found
}
