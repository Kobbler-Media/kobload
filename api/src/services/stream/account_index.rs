use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use dashmap::DashMap;
use futures::StreamExt;
use tracing::{debug, warn};
use uuid::Uuid;

use crate::debrid::{DebridProvider, DebridTorrent, ExistingTorrent, Lane, Throttled};

/// Listing a Real-Debrid account (`/torrents?limit=2500`) takes a couple of seconds, and RD
/// has no lookup by hash. A listing older than this is still used while a fresh one loads
/// in the background; our own adds and deletes are applied as they happen, so it only has
/// to catch torrents added from elsewhere.
const TTL: Duration = Duration::from_secs(600);
/// After a failed listing, how long until the next attempt.
const RETRY_AFTER_ERROR: Duration = Duration::from_secs(30);
const INFO_CONCURRENCY: usize = 4;

struct Snapshot {
    copies: HashMap<String, Vec<ExistingTorrent>>,
    refresh_after: Instant,
}

enum Change {
    Placed(String, ExistingTorrent),
    Removed(String, String),
}

#[derive(Default)]
struct Slot {
    snapshot: Mutex<Option<Snapshot>>,
    refresh: tokio::sync::Mutex<()>,
    refreshing: Mutex<Option<Lane>>,
    /// Our adds and deletes, re-applied over a listing that was requested before them.
    changes: Mutex<Vec<(Instant, Change)>>,
    /// Torrent details by id. RD fixes a torrent's file selection once, so which files a
    /// copy serves never changes.
    details: DashMap<String, DebridTorrent>,
}

impl Slot {
    fn copies(&self, info_hash: &str) -> Option<Vec<ExistingTorrent>> {
        let snapshot = self.snapshot.lock().unwrap_or_else(|e| e.into_inner());
        snapshot
            .as_ref()
            .map(|s| s.copies.get(info_hash).cloned().unwrap_or_default())
    }

    fn state(&self) -> Freshness {
        match self
            .snapshot
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            None => Freshness::Missing,
            Some(s) if s.refresh_after <= Instant::now() => Freshness::Stale,
            Some(_) => Freshness::Fresh,
        }
    }

    fn apply(&self, change: Change) {
        if let Some(s) = self
            .snapshot
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_mut()
        {
            apply(&mut s.copies, &change);
        }
        if let Change::Removed(_, id) = &change {
            self.details.remove(id);
        }
        self.changes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push((Instant::now(), change));
    }
}

#[derive(PartialEq)]
enum Freshness {
    Missing,
    Stale,
    Fresh,
}

fn apply(copies: &mut HashMap<String, Vec<ExistingTorrent>>, change: &Change) {
    match change {
        Change::Placed(hash, torrent) => {
            let list = copies.entry(hash.clone()).or_default();
            list.retain(|t| t.id != torrent.id);
            list.insert(0, torrent.clone());
            list.sort_by_key(|t| std::cmp::Reverse(t.usefulness()));
        }
        Change::Removed(hash, id) => {
            if let Some(list) = copies.get_mut(hash) {
                list.retain(|t| &t.id != id);
            }
        }
    }
}

static SLOTS: LazyLock<DashMap<Uuid, Arc<Slot>>> = LazyLock::new(DashMap::new);

fn slot(provider_id: Uuid) -> Arc<Slot> {
    SLOTS.entry(provider_id).or_default().clone()
}

/// Every copy of a hash on the account, best first. Only the first listing is waited for;
/// after that a stale one answers while a fresh one loads.
pub async fn copies(
    provider_id: Uuid,
    handler: &Throttled,
    http_client: &reqwest::Client,
    api_key: &str,
    info_hash: &str,
) -> Vec<ExistingTorrent> {
    let slot = slot(provider_id);
    match slot.state() {
        Freshness::Missing => refresh(&slot, handler, http_client, api_key).await,
        Freshness::Stale => spawn_refresh(slot.clone(), handler.provider(), http_client, api_key),
        Freshness::Fresh => {}
    }
    slot.copies(info_hash).unwrap_or_default()
}

/// Lists the account in the background so the first resolve after a search finds it ready.
pub fn prefetch(
    provider_id: Uuid,
    provider: Arc<dyn DebridProvider>,
    http_client: reqwest::Client,
    api_key: String,
) {
    let slot = slot(provider_id);
    if slot.state() != Freshness::Fresh {
        spawn_refresh(slot, provider, &http_client, &api_key);
    }
}

fn spawn_refresh(
    slot: Arc<Slot>,
    provider: Arc<dyn DebridProvider>,
    http_client: &reqwest::Client,
    api_key: &str,
) {
    if slot.refresh.try_lock().is_err() {
        return;
    }
    let http_client = http_client.clone();
    let api_key = api_key.to_string();
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
    if slot.state() == Freshness::Fresh {
        return;
    }
    *slot.refreshing.lock().unwrap_or_else(|e| e.into_inner()) = Some(handler.lane().clone());
    let started = Instant::now();
    let listed = handler.find_existing_torrents(http_client, api_key).await;
    {
        let mut snapshot = slot.snapshot.lock().unwrap_or_else(|e| e.into_inner());
        match listed {
            Ok(mut copies) => {
                let mut changes = slot.changes.lock().unwrap_or_else(|e| e.into_inner());
                changes.retain(|(at, _)| *at >= started);
                for (_, change) in changes.iter() {
                    apply(&mut copies, change);
                }
                let live: HashSet<&str> =
                    copies.values().flatten().map(|t| t.id.as_str()).collect();
                slot.details.retain(|id, _| live.contains(id.as_str()));
                *snapshot = Some(Snapshot {
                    copies,
                    refresh_after: Instant::now() + TTL,
                });
            }
            Err(e) => {
                warn!(error = %e, "Could not list debrid account; duplicates are possible until the next listing");
                let refresh_after = Instant::now() + RETRY_AFTER_ERROR;
                match snapshot.as_mut() {
                    Some(s) => s.refresh_after = refresh_after,
                    None => {
                        *snapshot = Some(Snapshot {
                            copies: HashMap::new(),
                            refresh_after,
                        })
                    }
                }
            }
        }
    }
    *slot.refreshing.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

pub struct Want {
    pub info_hash: String,
    pub file_name: Option<String>,
    pub provider_file_id: Option<i32>,
}

/// Which wants a finished copy on the account already serves, by index. Only when there is
/// no listing at all does this wait for one, and for at most `wait`.
/// A copy whose files could not be read in time counts as serving: playback checks again.
pub async fn serving(
    provider_id: Uuid,
    provider: Arc<dyn DebridProvider>,
    http_client: &reqwest::Client,
    api_key: &str,
    wants: &[Want],
    wait: Duration,
) -> HashSet<usize> {
    let slot = slot(provider_id);
    let handler = Throttled::new(provider, api_key, Lane::background());
    if slot.state() == Freshness::Missing {
        let _ = tokio::time::timeout(wait, refresh(&slot, &handler, http_client, api_key)).await;
    }
    let ready: Vec<(usize, Vec<ExistingTorrent>)> = wants
        .iter()
        .enumerate()
        .filter_map(|(i, w)| {
            let copies: Vec<ExistingTorrent> = slot
                .copies(&w.info_hash)?
                .into_iter()
                .filter(ExistingTorrent::is_finished)
                .collect();
            (!copies.is_empty()).then_some((i, copies))
        })
        .collect();
    let checks = futures::stream::iter(ready)
        .map(|(i, copies)| {
            let slot = &slot;
            let handler = &handler;
            async move {
                let want = &wants[i];
                for copy in copies {
                    let details = match slot.details.get(&copy.id) {
                        Some(d) => d.clone(),
                        None => {
                            let Ok(d) = tokio::time::timeout(
                                wait,
                                handler.get_torrent(http_client, api_key, &copy.id),
                            )
                            .await
                            else {
                                return Some(i);
                            };
                            match d {
                                Ok(d) => {
                                    slot.details.insert(copy.id.clone(), d.clone());
                                    d
                                }
                                Err(e) => {
                                    debug!(torrent_id = %copy.id, error = %e, "Could not read a copy's files");
                                    continue;
                                }
                            }
                        }
                    };
                    if !details.links.is_empty()
                        && details.serves(want.file_name.as_deref(), want.provider_file_id)
                    {
                        return Some(i);
                    }
                }
                None
            }
        })
        .buffer_unordered(INFO_CONCURRENCY)
        .collect::<Vec<_>>()
        .await;
    checks.into_iter().flatten().collect()
}

pub fn details(provider_id: Uuid, torrent_id: &str) -> Option<DebridTorrent> {
    slot(provider_id).details.get(torrent_id).map(|d| d.clone())
}

pub fn remember(provider_id: Uuid, torrent: &DebridTorrent) {
    slot(provider_id)
        .details
        .insert(torrent.id.clone(), torrent.clone());
}

pub fn placed(provider_id: Uuid, info_hash: &str, torrent: ExistingTorrent) {
    slot(provider_id).apply(Change::Placed(info_hash.to_string(), torrent));
}

pub fn removed(provider_id: Uuid, info_hash: &str, torrent_id: &str) {
    slot(provider_id).apply(Change::Removed(
        info_hash.to_string(),
        torrent_id.to_string(),
    ));
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;

    use super::*;
    use crate::debrid::{
        CachedAvailability, DebridAccountStatus, DebridError, DebridProviderType,
        DebridTorrentStatus, UnrestrictedLink,
    };

    /// An account holding one ready copy of `HASH`, whose listing takes `delay`.
    struct SlowAccount {
        listings: AtomicUsize,
        delay: Duration,
    }

    const HASH: &str = "aaaa";

    #[async_trait]
    impl DebridProvider for SlowAccount {
        fn provider_type(&self) -> DebridProviderType {
            DebridProviderType::RealDebrid
        }
        async fn check_availability(
            &self,
            _: &reqwest::Client,
            _: &str,
            _: &[&str],
        ) -> Result<Vec<CachedAvailability>, DebridError> {
            unreachable!()
        }
        async fn add_magnet(
            &self,
            _: &reqwest::Client,
            _: &str,
            _: &str,
        ) -> Result<String, DebridError> {
            unreachable!()
        }
        async fn select_files(
            &self,
            _: &reqwest::Client,
            _: &str,
            _: &str,
            _: &[i32],
        ) -> Result<(), DebridError> {
            unreachable!()
        }
        async fn get_torrent(
            &self,
            _: &reqwest::Client,
            _: &str,
            _: &str,
        ) -> Result<DebridTorrent, DebridError> {
            unreachable!()
        }
        async fn unrestrict_link(
            &self,
            _: &reqwest::Client,
            _: &str,
            _: &str,
        ) -> Result<UnrestrictedLink, DebridError> {
            unreachable!()
        }
        async fn get_account_status(
            &self,
            _: &reqwest::Client,
            _: &str,
        ) -> Result<DebridAccountStatus, DebridError> {
            unreachable!()
        }
        async fn find_existing_torrents(
            &self,
            _: &reqwest::Client,
            _: &str,
        ) -> Result<HashMap<String, Vec<ExistingTorrent>>, DebridError> {
            self.listings.fetch_add(1, Ordering::SeqCst);
            tokio::time::sleep(self.delay).await;
            Ok(HashMap::from([(
                HASH.to_string(),
                vec![ExistingTorrent {
                    id: "T1".to_string(),
                    status: DebridTorrentStatus::Ready,
                    has_links: true,
                }],
            )]))
        }
    }

    #[tokio::test]
    async fn a_stale_listing_answers_at_once_while_a_fresh_one_loads() {
        let provider_id = Uuid::new_v4();
        let account = Arc::new(SlowAccount {
            listings: AtomicUsize::new(0),
            delay: Duration::from_millis(300),
        });
        let handler = Throttled::new(account.clone(), "stale-test", Lane::background());
        let client = reqwest::Client::new();
        let first = copies(provider_id, &handler, &client, "stale-test", HASH).await;
        assert_eq!(first.len(), 1);

        if let Some(s) = slot(provider_id).snapshot.lock().unwrap().as_mut() {
            s.refresh_after = Instant::now();
        }
        let started = Instant::now();
        let stale = copies(provider_id, &handler, &client, "stale-test", HASH).await;
        assert_eq!(stale.len(), 1);
        assert!(
            started.elapsed() < Duration::from_millis(150),
            "{:?}",
            started.elapsed()
        );

        tokio::time::sleep(Duration::from_millis(600)).await;
        assert_eq!(
            account.listings.load(Ordering::SeqCst),
            2,
            "refreshed in the background"
        );
        assert!(slot(provider_id).state() == Freshness::Fresh);
    }

    #[tokio::test]
    async fn adds_made_while_a_listing_runs_survive_it() {
        let provider_id = Uuid::new_v4();
        let account = Arc::new(SlowAccount {
            listings: AtomicUsize::new(0),
            delay: Duration::from_millis(200),
        });
        prefetch(
            provider_id,
            account.clone(),
            reqwest::Client::new(),
            "racing".to_string(),
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
        placed(
            provider_id,
            "bbbb",
            ExistingTorrent {
                id: "T9".to_string(),
                status: DebridTorrentStatus::Ready,
                has_links: true,
            },
        );
        tokio::time::sleep(Duration::from_millis(400)).await;
        assert_eq!(
            slot(provider_id).copies("bbbb").unwrap().len(),
            1,
            "the listing started before the add and must not drop it"
        );
    }
}
