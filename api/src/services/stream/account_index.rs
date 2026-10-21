use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use dashmap::DashMap;
use tracing::warn;
use uuid::Uuid;

use crate::debrid::{DebridProvider, ExistingTorrent, Lane, Throttled};

/// Listing a Real-Debrid account (`/torrents?limit=500`) takes about a second, and RD has
/// no lookup by hash. Our own adds and deletes are applied as they happen, so the listing
/// only has to catch torrents added from elsewhere.
const TTL: Duration = Duration::from_secs(600);
/// After a failed listing, how long to proceed without one before trying again.
const RETRY_AFTER_ERROR: Duration = Duration::from_secs(30);

struct Snapshot {
    torrents: HashMap<String, ExistingTorrent>,
    expires: Instant,
}

#[derive(Default)]
struct Slot {
    snapshot: Mutex<Option<Snapshot>>,
    refresh: tokio::sync::Mutex<()>,
    refreshing: Mutex<Option<Lane>>,
}

impl Slot {
    fn fresh(&self, info_hash: &str) -> Option<Option<ExistingTorrent>> {
        let snapshot = self.snapshot.lock().unwrap_or_else(|e| e.into_inner());
        snapshot
            .as_ref()
            .filter(|s| s.expires > Instant::now())
            .map(|s| s.torrents.get(info_hash).cloned())
    }
}

static SLOTS: LazyLock<DashMap<Uuid, Arc<Slot>>> = LazyLock::new(DashMap::new);

fn slot(provider_id: Uuid) -> Arc<Slot> {
    SLOTS.entry(provider_id).or_default().clone()
}

pub async fn lookup(
    provider_id: Uuid,
    handler: &Throttled,
    http_client: &reqwest::Client,
    api_key: &str,
    info_hash: &str,
) -> Option<ExistingTorrent> {
    let slot = slot(provider_id);
    if let Some(found) = slot.fresh(info_hash) {
        return found;
    }
    refresh(&slot, handler, http_client, api_key).await;
    slot.fresh(info_hash).flatten()
}

/// Lists the account in the background so the first resolve after a search finds it ready.
pub fn prefetch(
    provider_id: Uuid,
    provider: Arc<dyn DebridProvider>,
    http_client: reqwest::Client,
    api_key: String,
) {
    let slot = slot(provider_id);
    if slot.fresh("").is_some() {
        return;
    }
    tokio::spawn(async move {
        let handler = Throttled::new(provider, &api_key, Lane::background());
        refresh(&slot, &handler, &http_client, &api_key).await;
    });
}

async fn refresh(slot: &Slot, handler: &Throttled, http_client: &reqwest::Client, api_key: &str) {
    if handler.lane().is_user()
        && let Some(running) = slot
            .refreshing
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
    {
        running.promote();
    }
    let _guard = slot.refresh.lock().await;
    if slot.fresh("").is_some() {
        return;
    }
    *slot.refreshing.lock().unwrap_or_else(|e| e.into_inner()) = Some(handler.lane().clone());
    let snapshot = match handler.find_existing_torrents(http_client, api_key).await {
        Ok(torrents) => Snapshot {
            torrents,
            expires: Instant::now() + TTL,
        },
        Err(e) => {
            warn!(error = %e, "Could not list debrid account; duplicates are possible until the next listing");
            Snapshot {
                torrents: HashMap::new(),
                expires: Instant::now() + RETRY_AFTER_ERROR,
            }
        }
    };
    *slot.snapshot.lock().unwrap_or_else(|e| e.into_inner()) = Some(snapshot);
    *slot.refreshing.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

pub fn record_added(provider_id: Uuid, info_hash: &str, torrent: ExistingTorrent) {
    if let Some(s) = slot(provider_id)
        .snapshot
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_mut()
    {
        s.torrents.insert(info_hash.to_string(), torrent);
    }
}

pub fn forget(provider_id: Uuid, info_hash: &str) {
    if let Some(s) = slot(provider_id)
        .snapshot
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .as_mut()
    {
        s.torrents.remove(info_hash);
    }
}
