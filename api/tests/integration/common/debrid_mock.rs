use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use kobload_api::debrid::provider::DebridProvider;
use kobload_api::debrid::types::ActiveSlots;
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
            if state.rate_limited_adds > 0 {
                state.rate_limited_adds -= 1;
                return Err(DebridError::RateLimited {
                    retry_after: Some(self.retry_after),
                });
            }
        }
        if self.release(&hash) == MockRelease::Refused {
            return Err(DebridError::Blocked);
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
        let cached = self.release(&hash) == MockRelease::Cached
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
            links,
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
        let path = self
            .files(&hash)
            .get(file_id - 1)
            .cloned()
            .unwrap_or_default();
        Ok(UnrestrictedLink {
            url: format!("https://mock.invalid/stream/{torrent_id}/{file_id}"),
            filename: path.rsplit('/').next().unwrap_or_default().to_string(),
            filesize: 2_000_000_000,
            mime_type: None,
            expires_at: None,
        })
    }

    async fn find_existing_torrents(
        &self,
        _: &reqwest::Client,
        _: &str,
    ) -> Result<HashMap<String, ExistingTorrent>, DebridError> {
        self.call("list_torrents", String::new()).await;
        let state = self.state.lock().unwrap();
        let mut newest: HashMap<String, &String> = HashMap::new();
        for (id, t) in &state.torrents {
            let entry = newest.entry(t.hash.clone()).or_insert(id);
            if torrent_number(id) > torrent_number(entry) {
                *entry = id;
            }
        }
        Ok(newest
            .into_iter()
            .map(|(hash, id)| {
                (
                    hash,
                    ExistingTorrent {
                        id: id.clone(),
                        status: DebridTorrentStatus::WaitingFileSelection,
                        has_links: false,
                    },
                )
            })
            .collect())
    }

    async fn find_existing_torrent(
        &self,
        client: &reqwest::Client,
        api_key: &str,
        info_hash: &str,
    ) -> Result<Option<ExistingTorrent>, DebridError> {
        Ok(self
            .find_existing_torrents(client, api_key)
            .await?
            .remove(&info_hash.to_lowercase()))
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
