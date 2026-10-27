use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use dashmap::DashMap;
use tracing::warn;

use super::provider::DebridProvider;
use super::types::{
    ActiveSlots, CachedAvailability, DebridAccountStatus, DebridError, DebridProviderType,
    DebridTorrent, ExistingTorrent, UnrestrictedLink, UsenetDownload,
};

/// Which queue a call waits in. Shared work (a probe a user later asks for) is promoted
/// in place, so whoever already holds the lane stops yielding to the user it now serves.
///
/// Downloads sit between the two: nobody is watching a spinner for them, so they wait out
/// a user's resolve, but a grab the user asked for still beats speculative warming.
#[derive(Clone, Debug)]
pub struct Lane(Arc<AtomicU8>);

const BACKGROUND: u8 = 0;
const DOWNLOAD: u8 = 1;
const USER: u8 = 2;

impl Lane {
    pub fn user() -> Self {
        Self(Arc::new(AtomicU8::new(USER)))
    }

    pub fn download() -> Self {
        Self(Arc::new(AtomicU8::new(DOWNLOAD)))
    }

    pub fn background() -> Self {
        Self(Arc::new(AtomicU8::new(BACKGROUND)))
    }

    pub fn promote(&self) {
        self.0.store(USER, Ordering::Relaxed);
    }

    pub fn is_user(&self) -> bool {
        self.priority() == USER
    }

    fn priority(&self) -> u8 {
        self.0.load(Ordering::Relaxed)
    }

    /// Tokens a call in this lane must leave in the bucket for the lanes above it.
    fn reserve(&self) -> f64 {
        match self.priority() {
            USER => 0.0,
            DOWNLOAD => USER_RESERVE,
            _ => USER_RESERVE + DOWNLOAD_RESERVE,
        }
    }
}

struct Limits {
    per_sec: f64,
    burst: f64,
}

/// Real-Debrid documents 250 requests/minute per account. 3/s (180/min) leaves room for
/// the download client, which shares the key but not this gate.
fn limits(provider: DebridProviderType) -> Limits {
    match provider {
        DebridProviderType::RealDebrid => Limits {
            per_sec: 3.0,
            burst: 8.0,
        },
        _ => Limits {
            per_sec: 5.0,
            burst: 10.0,
        },
    }
}

/// Tokens lower lanes leave in the bucket, so a user's first calls never queue behind a
/// burst of grabs or warming, and a grab never queues behind warming.
const USER_RESERVE: f64 = 3.0;
const DOWNLOAD_RESERVE: f64 = 2.0;
const YIELD_TICK: Duration = Duration::from_millis(25);
const BASE_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(30);

struct Bucket {
    tokens: f64,
    refilled: Instant,
    paused_until: Option<Instant>,
    backoff: Duration,
}

pub struct Gate {
    limits: Limits,
    bucket: Mutex<Bucket>,
    users: AtomicUsize,
}

static GATES: LazyLock<DashMap<String, Arc<Gate>>> = LazyLock::new(DashMap::new);

/// One gate per provider account; the limit is per API key, not per caller.
pub fn gate(provider: DebridProviderType, api_key: &str) -> Arc<Gate> {
    GATES
        .entry(format!("{}:{api_key}", provider.as_str()))
        .or_insert_with(|| {
            let limits = limits(provider);
            Arc::new(Gate {
                bucket: Mutex::new(Bucket {
                    tokens: limits.burst,
                    refilled: Instant::now(),
                    paused_until: None,
                    backoff: BASE_BACKOFF,
                }),
                limits,
                users: AtomicUsize::new(0),
            })
        })
        .clone()
}

pub struct UserSession(Arc<Gate>);

impl Drop for UserSession {
    fn drop(&mut self) {
        self.0.users.fetch_sub(1, Ordering::Relaxed);
    }
}

impl Gate {
    /// Calls below the user lane on this account wait while any session is alive.
    pub fn user_session(self: &Arc<Self>) -> UserSession {
        self.users.fetch_add(1, Ordering::Relaxed);
        UserSession(self.clone())
    }

    async fn acquire(&self, lane: &Lane) {
        loop {
            let user = lane.is_user();
            if !user && self.users.load(Ordering::Relaxed) > 0 {
                tokio::time::sleep(YIELD_TICK).await;
                continue;
            }
            let wait = {
                let mut b = self.bucket.lock().unwrap_or_else(|e| e.into_inner());
                let now = Instant::now();
                let elapsed = now.duration_since(b.refilled).as_secs_f64();
                b.tokens = (b.tokens + elapsed * self.limits.per_sec).min(self.limits.burst);
                b.refilled = now;
                match b.paused_until {
                    Some(until) if until > now => until - now,
                    _ => {
                        let need = 1.0 + lane.reserve();
                        if b.tokens >= need {
                            b.tokens -= 1.0;
                            return;
                        }
                        Duration::from_secs_f64((need - b.tokens) / self.limits.per_sec)
                    }
                }
            };
            tokio::time::sleep(if user { wait } else { wait.min(YIELD_TICK) }).await;
        }
    }

    /// Returns whether the call was rate limited, after pausing every caller on the account.
    fn observe<T>(&self, result: &Result<T, DebridError>) -> bool {
        let mut b = self.bucket.lock().unwrap_or_else(|e| e.into_inner());
        match result {
            Err(DebridError::RateLimited { retry_after }) => {
                let pause = retry_after.unwrap_or(b.backoff);
                warn!(
                    pause_ms = pause.as_millis() as u64,
                    "Debrid rate limited, pausing account"
                );
                b.paused_until = Some(Instant::now() + pause);
                b.backoff = (b.backoff * 2).min(MAX_BACKOFF);
                b.tokens = 0.0;
                true
            }
            Ok(_) => {
                b.backoff = BASE_BACKOFF;
                false
            }
            Err(_) => false,
        }
    }
}

/// A provider whose every call goes through the account's [`Gate`]. A 429 is retried
/// after the provider's Retry-After, since it means the request was never processed.
pub struct Throttled {
    inner: Arc<dyn DebridProvider>,
    gate: Arc<Gate>,
    lane: Lane,
}

impl Throttled {
    pub fn new(inner: Arc<dyn DebridProvider>, api_key: &str, lane: Lane) -> Self {
        let gate = gate(inner.provider_type(), api_key);
        Self { inner, gate, lane }
    }

    pub fn lane(&self) -> &Lane {
        &self.lane
    }

    pub fn provider(&self) -> Arc<dyn DebridProvider> {
        self.inner.clone()
    }

    pub fn gate(&self) -> &Arc<Gate> {
        &self.gate
    }

    async fn run<'a, T, F, Fut>(&'a self, call: F) -> Result<T, DebridError>
    where
        F: Fn() -> Fut,
        Fut: Future<Output = Result<T, DebridError>> + 'a,
    {
        let attempts = if self.lane.priority() == BACKGROUND {
            2
        } else {
            3
        };
        let mut made = 0;
        loop {
            self.gate.acquire(&self.lane).await;
            let result = call().await;
            made += 1;
            if !self.gate.observe(&result) || made >= attempts {
                return result;
            }
        }
    }
}

#[async_trait]
impl DebridProvider for Throttled {
    fn provider_type(&self) -> DebridProviderType {
        self.inner.provider_type()
    }

    fn supports_usenet(&self) -> bool {
        self.inner.supports_usenet()
    }

    fn max_hashes_per_batch(&self) -> usize {
        self.inner.max_hashes_per_batch()
    }

    async fn check_availability(
        &self,
        http_client: &reqwest::Client,
        api_key: &str,
        hashes: &[&str],
    ) -> Result<Vec<CachedAvailability>, DebridError> {
        self.run(|| self.inner.check_availability(http_client, api_key, hashes))
            .await
    }

    async fn add_magnet(
        &self,
        http_client: &reqwest::Client,
        api_key: &str,
        magnet: &str,
    ) -> Result<String, DebridError> {
        self.run(|| self.inner.add_magnet(http_client, api_key, magnet))
            .await
    }

    async fn select_files(
        &self,
        http_client: &reqwest::Client,
        api_key: &str,
        torrent_id: &str,
        file_ids: &[i32],
    ) -> Result<(), DebridError> {
        self.run(|| {
            self.inner
                .select_files(http_client, api_key, torrent_id, file_ids)
        })
        .await
    }

    async fn get_torrent(
        &self,
        http_client: &reqwest::Client,
        api_key: &str,
        torrent_id: &str,
    ) -> Result<DebridTorrent, DebridError> {
        self.run(|| self.inner.get_torrent(http_client, api_key, torrent_id))
            .await
    }

    async fn unrestrict_link(
        &self,
        http_client: &reqwest::Client,
        api_key: &str,
        link: &str,
    ) -> Result<UnrestrictedLink, DebridError> {
        self.run(|| self.inner.unrestrict_link(http_client, api_key, link))
            .await
    }

    async fn find_existing_torrents(
        &self,
        http_client: &reqwest::Client,
        api_key: &str,
    ) -> Result<HashMap<String, Vec<ExistingTorrent>>, DebridError> {
        self.run(|| self.inner.find_existing_torrents(http_client, api_key))
            .await
    }

    async fn delete_torrent(
        &self,
        http_client: &reqwest::Client,
        api_key: &str,
        torrent_id: &str,
    ) -> Result<(), DebridError> {
        self.run(|| self.inner.delete_torrent(http_client, api_key, torrent_id))
            .await
    }

    async fn active_slots(
        &self,
        http_client: &reqwest::Client,
        api_key: &str,
    ) -> Result<Option<ActiveSlots>, DebridError> {
        self.run(|| self.inner.active_slots(http_client, api_key))
            .await
    }

    async fn check_usenet_cached(
        &self,
        http_client: &reqwest::Client,
        api_key: &str,
        ids: &[&str],
    ) -> Result<Vec<bool>, DebridError> {
        self.run(|| self.inner.check_usenet_cached(http_client, api_key, ids))
            .await
    }

    async fn add_nzb(
        &self,
        http_client: &reqwest::Client,
        api_key: &str,
        nzb_url: &str,
    ) -> Result<String, DebridError> {
        self.run(|| self.inner.add_nzb(http_client, api_key, nzb_url))
            .await
    }

    async fn get_usenet_download(
        &self,
        http_client: &reqwest::Client,
        api_key: &str,
        id: &str,
    ) -> Result<UsenetDownload, DebridError> {
        self.run(|| self.inner.get_usenet_download(http_client, api_key, id))
            .await
    }

    async fn get_account_status(
        &self,
        http_client: &reqwest::Client,
        api_key: &str,
    ) -> Result<DebridAccountStatus, DebridError> {
        self.run(|| self.inner.get_account_status(http_client, api_key))
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fresh_gate() -> Arc<Gate> {
        gate(
            DebridProviderType::RealDebrid,
            &uuid::Uuid::new_v4().to_string(),
        )
    }

    #[tokio::test]
    async fn each_lane_leaves_a_reserve_for_the_ones_above() {
        let gate = fresh_gate();
        let background = Lane::background();
        let taken = async {
            let mut n = 0;
            while tokio::time::timeout(Duration::from_millis(5), gate.acquire(&background))
                .await
                .is_ok()
            {
                n += 1;
            }
            n
        }
        .await;
        assert_eq!(taken, 8 - (USER_RESERVE + DOWNLOAD_RESERVE) as usize);
        let started = Instant::now();
        for _ in 0..DOWNLOAD_RESERVE as usize {
            gate.acquire(&Lane::download()).await;
        }
        for _ in 0..USER_RESERVE as usize {
            gate.acquire(&Lane::user()).await;
        }
        assert!(started.elapsed() < Duration::from_millis(20));
    }

    #[tokio::test]
    async fn rate_limit_pauses_every_caller_for_retry_after() {
        let gate = fresh_gate();
        let limited: Result<(), DebridError> = Err(DebridError::RateLimited {
            retry_after: Some(Duration::from_millis(200)),
        });
        assert!(gate.observe(&limited));
        let started = Instant::now();
        gate.acquire(&Lane::user()).await;
        assert!(started.elapsed() >= Duration::from_millis(200));
    }
}
