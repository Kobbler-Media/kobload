use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use kobload_api::debrid::provider::DebridProvider;
use kobload_api::debrid::rd_names;
use kobload_api::debrid::types::{ActiveSlots, AddPauses};
use kobload_api::debrid::{
    CachedAvailability, DebridAccountStatus, DebridError, DebridFile, DebridProviderResolver,
    DebridProviderType, DebridTorrent, DebridTorrentStatus, ExistingTorrent, UnrestrictedLink,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MockRelease {
    Cached,
    Uncached,
    Refused,
}

#[derive(Debug, Clone)]
pub struct MockCall {
    pub op: &'static str,
    pub hash: String,
    pub started: Instant,
}

struct MockTorrent {
    hash: String,
    selected: Vec<i32>,
}

#[derive(Default)]
struct MockState {
    releases: HashMap<String, MockRelease>,
    packs: HashMap<String, Vec<String>>,
    uncached_files: HashMap<String, HashSet<i32>>,
    torrents: HashMap<String, MockTorrent>,
    next_id: u32,
    calls: Vec<MockCall>,
    rate_limited_adds: u32,
    progress: HashMap<String, f32>,
    listing_delay: Duration,
    lost: HashSet<(String, i32)>,
    unseen_gets: u32,
    linkless: HashSet<String>,
    magnets: Vec<String>,
    archived: HashSet<String>,
    add_pauses: Option<AddPauses>,
    refused_adds: u32,
    active_limit: Option<usize>,
    served_as: HashMap<String, (String, String)>,
}

/// A stateful in-memory debrid account. Every call is logged with the hash it concerns.
pub struct MockDebrid {
    state: Mutex<MockState>,
    latency: Duration,
    pub retry_after: Duration,
}

impl MockDebrid {
    pub fn new(latency: Duration) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::default(),
            latency,
            retry_after: Duration::from_millis(300),
        })
    }

    pub fn with_releases(self: Arc<Self>, releases: &[(&str, MockRelease)]) -> Arc<Self> {
        let mut state = self.state.lock().unwrap();
        for (hash, r) in releases {
            state.releases.insert(hash.to_string(), *r);
        }
        drop(state);
        self
    }

    /// Files get ids 1.. in the order given, like Real-Debrid's.
    pub fn with_pack(self: Arc<Self>, hash: &str, paths: &[&str]) -> Arc<Self> {
        self.state.lock().unwrap().packs.insert(
            hash.to_string(),
            paths.iter().map(|p| p.to_string()).collect(),
        );
        self
    }

    /// These files of an otherwise cached release would have to download first.
    pub fn with_uncached_files(self: Arc<Self>, hash: &str, ids: &[i32]) -> Arc<Self> {
        self.state
            .lock()
            .unwrap()
            .uncached_files
            .insert(hash.to_string(), ids.iter().copied().collect());
        self
    }

    /// A torrent already on the account, as an earlier selection left it.
    pub fn with_torrent(self: Arc<Self>, hash: &str, selected: &[i32]) -> Arc<Self> {
        let mut state = self.state.lock().unwrap();
        state.next_id += 1;
        let id = format!("T{}", state.next_id);
        state.torrents.insert(
            id,
            MockTorrent {
                hash: hash.to_string(),
                selected: selected.to_vec(),
            },
        );
        drop(state);
        self
    }

    /// Real-Debrid has lost this file from every copy held now: its link answers
    /// `hoster_unavailable`, while a fresh add of the hash serves it again.
    pub fn lose_held_file(&self, hash: &str, file_id: i32) {
        let mut state = self.state.lock().unwrap();
        let held: Vec<String> = state
            .torrents
            .iter()
            .filter(|(_, t)| t.hash == hash)
            .map(|(id, _)| id.clone())
            .collect();
        state.lost.extend(held.into_iter().map(|id| (id, file_id)));
    }

    /// The account is emptied behind kobload's back, as a user clearing it would.
    pub fn forget_torrents(&self) {
        self.state.lock().unwrap().torrents.clear();
    }

    pub fn holds(&self, torrent_id: &str) -> bool {
        self.state.lock().unwrap().torrents.contains_key(torrent_id)
    }

    /// The file ids selected on each torrent of this hash, oldest torrent first.
    pub fn selections(&self, hash: &str) -> Vec<Vec<i32>> {
        let state = self.state.lock().unwrap();
        let mut torrents: Vec<(&String, &MockTorrent)> = state
            .torrents
            .iter()
            .filter(|(_, t)| t.hash == hash)
            .collect();
        torrents.sort_by_key(|(id, _)| torrent_number(id));
        torrents
            .into_iter()
            .map(|(_, t)| t.selected.clone())
            .collect()
    }

    pub fn set_release(&self, hash: &str, release: MockRelease) {
        self.state
            .lock()
            .unwrap()
            .releases
            .insert(hash.to_string(), release);
    }

    pub fn set_progress(&self, hash: &str, progress: f32) {
        self.state
            .lock()
            .unwrap()
            .progress
            .insert(hash.to_string(), progress);
    }

    /// Account listings take this long on top of the call latency, like a large RD account.
    pub fn slow_listing(&self, delay: Duration) {
        self.state.lock().unwrap().listing_delay = delay;
    }

    /// RD is not read-your-writes: a torrent just added can 404 for a moment.
    pub fn not_found_next_gets(&self, n: u32) {
        self.state.lock().unwrap().unseen_gets = n;
    }

    /// Copies held now report finished with no links, as RD's sometimes do.
    pub fn strip_held_links(&self, hash: &str) {
        let mut state = self.state.lock().unwrap();
        let held: Vec<String> = state
            .torrents
            .iter()
            .filter(|(_, t)| t.hash == hash)
            .map(|(id, _)| id.clone())
            .collect();
        state.linkless.extend(held);
    }

    /// A selection of more than one file unrestricts to one archive of them all.
    pub fn archive_multi_selections(&self, hash: &str) {
        self.state.lock().unwrap().archived.insert(hash.to_string());
    }

    pub fn magnets(&self) -> Vec<String> {
        self.state.lock().unwrap().magnets.clone()
    }

    /// Pauses adds the way Real-Debrid does, with a hold short enough for a test.
    pub fn with_add_pauses(self: Arc<Self>, hold: Duration) -> Arc<Self> {
        self.state.lock().unwrap().add_pauses = Some(AddPauses {
            hold,
            burst: 4,
            spacing: Duration::from_secs(45),
            refuses_name: rd_names::refuses,
        });
        self
    }

    /// Adds beyond this many torrents still downloading answer "too many active".
    pub fn with_active_limit(self: Arc<Self>, n: usize) -> Arc<Self> {
        self.state.lock().unwrap().active_limit = Some(n);
        self
    }

    /// The next adds are refused as infringing, whatever the hash, as during a pause.
    pub fn refuse_next_adds(&self, n: u32) {
        self.state.lock().unwrap().refused_adds = n;
    }

    pub fn serve_as(&self, hash: &str, filename: &str, mime: &str) {
        self.state
            .lock()
            .unwrap()
            .served_as
            .insert(hash.to_string(), (filename.to_string(), mime.to_string()));
    }

    pub fn rate_limit_next_adds(&self, n: u32) {
        self.state.lock().unwrap().rate_limited_adds = n;
    }

    pub fn calls(&self) -> Vec<MockCall> {
        self.state.lock().unwrap().calls.clone()
    }

    pub fn count(&self, op: &str, hash: Option<&str>) -> usize {
        self.calls()
            .iter()
            .filter(|c| c.op == op && hash.is_none_or(|h| c.hash == h))
            .count()
    }

    pub fn torrents_for(&self, hash: &str) -> usize {
        self.state
            .lock()
            .unwrap()
            .torrents
            .values()
            .filter(|t| t.hash == hash)
            .count()
    }

    pub fn resolver(self: &Arc<Self>) -> Arc<dyn DebridProviderResolver> {
        Arc::new(MockResolver(self.clone()))
    }

    async fn call(&self, op: &'static str, hash: String) {
        self.state.lock().unwrap().calls.push(MockCall {
            op,
            hash,
            started: Instant::now(),
        });
        tokio::time::sleep(self.latency).await;
    }

    fn hash_of(&self, torrent_id: &str) -> String {
        self.state
            .lock()
            .unwrap()
            .torrents
            .get(torrent_id)
            .map(|t| t.hash.clone())
            .unwrap_or_default()
    }

    fn files(&self, hash: &str) -> Vec<String> {
        self.state
            .lock()
            .unwrap()
            .packs
            .get(hash)
            .cloned()
            .unwrap_or_else(|| vec![format!("{hash}/video.mkv")])
    }

    fn release(&self, hash: &str) -> MockRelease {
        self.state
            .lock()
            .unwrap()
            .releases
            .get(hash)
            .copied()
            .unwrap_or(MockRelease::Cached)
    }
}

fn torrent_number(id: &str) -> u32 {
    id.trim_start_matches('T').parse().unwrap_or(0)
}

pub fn magnet_hash(magnet: &str) -> String {
    magnet
        .split("btih:")
        .nth(1)
        .unwrap_or_default()
        .split('&')
        .next()
        .unwrap_or_default()
        .to_lowercase()
}

#[async_trait]
impl DebridProvider for MockDebrid {
    fn provider_type(&self) -> DebridProviderType {
        DebridProviderType::RealDebrid
    }

    fn add_pauses(&self) -> Option<AddPauses> {
        self.state.lock().unwrap().add_pauses
    }

    async fn check_availability(
        &self,
        _: &reqwest::Client,
        _: &str,
        _: &[&str],
    ) -> Result<Vec<CachedAvailability>, DebridError> {
        Ok(Vec::new())
    }

    async fn add_magnet(
        &self,
        _: &reqwest::Client,
        _: &str,
        magnet: &str,
    ) -> Result<String, DebridError> {
        let hash = magnet_hash(magnet);
        self.call("add_magnet", hash.clone()).await;
        {
            let mut state = self.state.lock().unwrap();
            state.magnets.push(magnet.to_string());
            if state.rate_limited_adds > 0 {
                state.rate_limited_adds -= 1;
                return Err(DebridError::RateLimited {
                    retry_after: Some(self.retry_after),
                });
            }
            if state.refused_adds > 0 {
                state.refused_adds -= 1;
                return Err(DebridError::Blocked);
            }
        }
        if self.release(&hash) == MockRelease::Refused {
            return Err(DebridError::Blocked);
        }
        let limit = self.state.lock().unwrap().active_limit;
        if let Some(limit) = limit {
            let downloading = {
                let state = self.state.lock().unwrap();
                state
                    .torrents
                    .values()
                    .map(|t| t.hash.clone())
                    .collect::<Vec<_>>()
            };
            let active = downloading
                .iter()
                .filter(|h| self.release(h) == MockRelease::Uncached)
                .count();
            if active >= limit {
                return Err(DebridError::TooManyActive);
            }
        }
        let mut state = self.state.lock().unwrap();
        state.next_id += 1;
        let id = format!("T{}", state.next_id);
        state.torrents.insert(
            id.clone(),
            MockTorrent {
                hash,
                selected: Vec::new(),
            },
        );
        Ok(id)
    }

    async fn select_files(
        &self,
        _: &reqwest::Client,
        _: &str,
        torrent_id: &str,
        ids: &[i32],
    ) -> Result<(), DebridError> {
        let hash = self.hash_of(torrent_id);
        self.call("select_files", hash.clone()).await;
        let all: Vec<i32> = (1..=self.files(&hash).len() as i32).collect();
        match self.state.lock().unwrap().torrents.get_mut(torrent_id) {
            Some(t) if !t.selected.is_empty() => Err(DebridError::Status(
                reqwest::StatusCode::FORBIDDEN,
                "files already selected".to_string(),
            )),
            Some(t) => {
                t.selected = if ids.is_empty() { all } else { ids.to_vec() };
                t.selected.sort_unstable();
                Ok(())
            }
            None => Err(DebridError::NotFound),
        }
    }

    async fn get_torrent(
        &self,
        _: &reqwest::Client,
        _: &str,
        torrent_id: &str,
    ) -> Result<DebridTorrent, DebridError> {
        let hash = self.hash_of(torrent_id);
        self.call("get_torrent", hash.clone()).await;
        {
            let mut state = self.state.lock().unwrap();
            if state.unseen_gets > 0 {
                state.unseen_gets -= 1;
                return Err(DebridError::NotFound);
            }
        }
        let linkless = self.state.lock().unwrap().linkless.contains(torrent_id);
        let selected = match self.state.lock().unwrap().torrents.get(torrent_id) {
            Some(t) => t.selected.clone(),
            None => return Err(DebridError::NotFound),
        };
        let uncached = self
            .state
            .lock()
            .unwrap()
            .uncached_files
            .get(&hash)
            .cloned()
            .unwrap_or_default();
        // Like Real-Debrid, a refusal only stops the add; a copy already held still serves.
        let cached = self.release(&hash) != MockRelease::Uncached
            && !selected.iter().any(|id| uncached.contains(id));
        let (status, links) = match (selected.is_empty(), cached) {
            (true, _) => (DebridTorrentStatus::WaitingFileSelection, Vec::new()),
            (false, true) => (
                DebridTorrentStatus::Ready,
                selected
                    .iter()
                    .map(|id| format!("https://mock.invalid/link/{torrent_id}/{id}"))
                    .collect(),
            ),
            (false, false) => (
                DebridTorrentStatus::Downloading {
                    progress: self
                        .state
                        .lock()
                        .unwrap()
                        .progress
                        .get(&hash)
                        .copied()
                        .unwrap_or(0.0),
                },
                Vec::new(),
            ),
        };
        let files = self
            .files(&hash)
            .into_iter()
            .enumerate()
            .map(|(i, path)| {
                let id = i as i32 + 1;
                DebridFile {
                    id,
                    path: format!("/{path}"),
                    size: if path.to_lowercase().contains("sample") {
                        50_000_000
                    } else {
                        2_000_000_000
                    },
                    selected: selected.contains(&id),
                }
            })
            .collect();
        Ok(DebridTorrent {
            id: torrent_id.to_string(),
            status,
            files,
            links: if linkless { Vec::new() } else { links },
        })
    }

    async fn unrestrict_link(
        &self,
        _: &reqwest::Client,
        _: &str,
        link: &str,
    ) -> Result<UnrestrictedLink, DebridError> {
        let mut parts = link.rsplit('/');
        let file_id: usize = parts.next().unwrap_or_default().parse().unwrap_or(1);
        let torrent_id = parts.next().unwrap_or_default().to_string();
        let hash = self.hash_of(&torrent_id);
        self.call("unrestrict_link", hash.clone()).await;
        if self
            .state
            .lock()
            .unwrap()
            .lost
            .contains(&(torrent_id.clone(), file_id as i32))
        {
            return Err(DebridError::HosterUnavailable);
        }
        let path = self
            .files(&hash)
            .get(file_id - 1)
            .cloned()
            .unwrap_or_default();
        let multi = {
            let state = self.state.lock().unwrap();
            state.archived.contains(&hash)
                && state
                    .torrents
                    .get(&torrent_id)
                    .is_some_and(|t| t.selected.len() > 1)
        };
        if multi {
            return Ok(UnrestrictedLink {
                url: format!("https://mock.invalid/stream/{torrent_id}/all.rar"),
                filename: "Show.S01.rar".to_string(),
                filesize: 6_000_000_000,
                mime_type: None,
                expires_at: None,
            });
        }
        let served_as = self.state.lock().unwrap().served_as.get(&hash).cloned();
        let (filename, mime_type) = match served_as {
            Some((name, mime)) => (name, Some(mime)),
            None => (
                path.rsplit('/').next().unwrap_or_default().to_string(),
                None,
            ),
        };
        Ok(UnrestrictedLink {
            url: format!("https://mock.invalid/stream/{torrent_id}/{file_id}"),
            filename,
            filesize: 2_000_000_000,
            mime_type,
            expires_at: None,
        })
    }

    async fn find_existing_torrents(
        &self,
        _: &reqwest::Client,
        _: &str,
    ) -> Result<HashMap<String, Vec<ExistingTorrent>>, DebridError> {
        self.call("list_torrents", String::new()).await;
        let listing_delay = self.state.lock().unwrap().listing_delay;
        tokio::time::sleep(listing_delay).await;
        let mut copies: Vec<(u32, String, String, bool)> = {
            let state = self.state.lock().unwrap();
            state
                .torrents
                .iter()
                .map(|(id, t)| {
                    (
                        torrent_number(id),
                        t.hash.clone(),
                        id.clone(),
                        !t.selected.is_empty(),
                    )
                })
                .collect()
        };
        copies.sort_by_key(|(n, ..)| std::cmp::Reverse(*n));
        let mut out: HashMap<String, Vec<ExistingTorrent>> = HashMap::new();
        for (_, hash, id, selected) in copies {
            let ready = selected && self.release(&hash) != MockRelease::Uncached;
            // Real-Debrid's listing leaves the links off nearly every finished torrent.
            out.entry(hash).or_default().push(ExistingTorrent {
                id,
                status: if ready {
                    DebridTorrentStatus::Ready
                } else {
                    DebridTorrentStatus::WaitingFileSelection
                },
                has_links: false,
            });
        }
        for list in out.values_mut() {
            list.sort_by_key(|t| std::cmp::Reverse(t.usefulness()));
        }
        Ok(out)
    }

    async fn delete_torrent(
        &self,
        _: &reqwest::Client,
        _: &str,
        torrent_id: &str,
    ) -> Result<(), DebridError> {
        self.call("delete_torrent", self.hash_of(torrent_id)).await;
        self.state.lock().unwrap().torrents.remove(torrent_id);
        Ok(())
    }

    async fn active_slots(
        &self,
        _: &reqwest::Client,
        _: &str,
    ) -> Result<Option<ActiveSlots>, DebridError> {
        self.call("active_slots", String::new()).await;
        Ok(None)
    }

    async fn get_account_status(
        &self,
        _: &reqwest::Client,
        _: &str,
    ) -> Result<DebridAccountStatus, DebridError> {
        Err(DebridError::UnsupportedOperation)
    }
}

struct MockResolver(Arc<MockDebrid>);

impl DebridProviderResolver for MockResolver {
    fn get(&self, _: &DebridProviderType) -> Arc<dyn DebridProvider> {
        self.0.clone()
    }
}
