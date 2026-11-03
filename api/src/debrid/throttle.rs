use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::{AtomicU8, AtomicUsize, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use dashmap::DashMap;
use tracing::warn;

use super::magnet;
use super::provider::DebridProvider;
use super::types::{
    ActiveSlots, AddPauses, CachedAvailability, DebridAccountStatus, DebridError,
    DebridProviderType, DebridTorrent, ExistingTorrent, UnrestrictedLink, UsenetDownload,
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

    fn reserve(&self, limits: &Limits) -> f64 {
        match self.priority() {
            USER => 0.0,
            DOWNLOAD => limits.user_reserve,
            _ => limits.user_reserve + limits.download_reserve,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Call {
    Add,
    Unrestrict,
    Other,
}

/// `user_reserve` / `download_reserve`: tokens lower lanes leave in the bucket, so a
/// user's first calls never queue behind a burst of grabs or warming, and a grab never
/// queues behind warming.
struct Limits {
    per_sec: f64,
    burst: f64,
    user_reserve: f64,
    download_reserve: f64,
}

/// Real-Debrid documents 250 requests/minute per account. 3/s (180/min) leaves room for
/// the download client, which shares the key but not this gate.
fn limits(provider: DebridProviderType) -> Limits {
    match provider {
        DebridProviderType::RealDebrid => Limits {
            per_sec: 3.0,
            burst: 8.0,
            user_reserve: 3.0,
            download_reserve: 2.0,
        },
        _ => Limits {
            per_sec: 5.0,
            burst: 10.0,
            user_reserve: 3.0,
            download_reserve: 2.0,
        },
    }
}

/// Real-Debrid throttles `addMagnet` well below its documented limit: on a live account
/// 7-15 adds inside ten seconds drew 429s while 19 spread over a minute did not.
fn add_limits(provider: DebridProviderType) -> Option<Limits> {
    (provider == DebridProviderType::RealDebrid).then_some(Limits {
        per_sec: 0.4,
        burst: 5.0,
        user_reserve: 2.0,
        download_reserve: 1.0,
    })
}

/// Debrid Media Manager saw `/unrestrict/link` answer error 34 after only a handful of
/// calls a few seconds apart, well inside the account's overall limit.
fn unrestrict_limits(provider: DebridProviderType) -> Option<Limits> {
    (provider == DebridProviderType::RealDebrid).then_some(Limits {
        per_sec: 0.5,
        burst: 4.0,
        user_reserve: 2.0,
        download_reserve: 1.0,
    })
}

/// A user's add never waits for a quiet account, but spends from it; a download needs a
/// little room left, and warming only adds once nothing has been added for a while.
fn quiet_limits(pauses: &AddPauses) -> Limits {
    let burst = f64::from(pauses.burst.max(2));
    Limits {
        per_sec: 1.0 / pauses.spacing.as_secs_f64(),
        burst,
        user_reserve: 1.0,
        download_reserve: burst - 2.0,
    }
}

const YIELD_TICK: Duration = Duration::from_millis(25);
const BASE_BACKOFF: Duration = Duration::from_secs(1);
const MAX_BACKOFF: Duration = Duration::from_secs(30);
const BACKGROUND_ATTEMPTS: u32 = 2;
const DOWNLOAD_ATTEMPTS: u32 = 3;
/// kobapp gives up on its whole walk at the first `rate_limited`, so a user call outwaits
/// the throttle for this long before reporting one.
const USER_RETRY_WINDOW: Duration = Duration::from_secs(30);
/// Warming that would have to wait longer than this for an add gives up instead: by the
/// time it got its turn the user has usually picked something.
const BACKGROUND_ADD_PATIENCE: Duration = Duration::from_secs(2);

struct Bucket {
    limits: Limits,
    tokens: f64,
    refilled: Instant,
}

impl Bucket {
    fn new(limits: Limits) -> Self {
        Self {
            tokens: limits.burst,
            refilled: Instant::now(),
            limits,
        }
    }

    fn refill(&mut self, now: Instant) {
        let elapsed = now.duration_since(self.refilled).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.limits.per_sec).min(self.limits.burst);
        self.refilled = now;
    }

    /// How long until this lane may spend a token; zero once it can.
    fn shortfall(&self, lane: &Lane) -> Duration {
        let need = 1.0 + lane.reserve(&self.limits);
        Duration::from_secs_f64(((need - self.tokens) / self.limits.per_sec).max(0.0))
    }
}

struct Buckets {
    calls: Bucket,
    adds: Option<Bucket>,
    unrestricts: Option<Bucket>,
    quiet: Option<Bucket>,
    paused_until: Option<Instant>,
    adds_held_until: Option<Instant>,
    backoff: Duration,
}

impl Buckets {
    fn refill(&mut self, now: Instant) {
        self.calls.refill(now);
        for bucket in [&mut self.adds, &mut self.unrestricts, &mut self.quiet]
            .into_iter()
            .flatten()
        {
            bucket.refill(now);
        }
    }

    fn call_bucket(&mut self, call: Call) -> Option<&mut Bucket> {
        match call {
            Call::Add => self.adds.as_mut(),
            Call::Unrestrict => self.unrestricts.as_mut(),
            Call::Other => None,
        }
    }
}

pub struct Gate {
    buckets: Mutex<Buckets>,
    users: AtomicUsize,
}

static GATES: LazyLock<DashMap<String, Arc<Gate>>> = LazyLock::new(DashMap::new);

/// One gate per provider account; the limit is per API key, not per caller.
pub fn gate(provider: DebridProviderType, api_key: &str) -> Arc<Gate> {
    GATES
        .entry(format!("{}:{api_key}", provider.as_str()))
        .or_insert_with(|| {
            Arc::new(Gate {
                buckets: Mutex::new(Buckets {
                    calls: Bucket::new(limits(provider)),
                    adds: add_limits(provider).map(Bucket::new),
                    unrestricts: unrestrict_limits(provider).map(Bucket::new),
                    quiet: None,
                    paused_until: None,
                    adds_held_until: None,
                    backoff: BASE_BACKOFF,
                }),
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

    async fn acquire(
        &self,
        lane: &Lane,
        call: Call,
        pauses: Option<&AddPauses>,
    ) -> Result<(), DebridError> {
        loop {
            let user = lane.is_user();
            if !user && self.users.load(Ordering::Relaxed) > 0 {
                tokio::time::sleep(YIELD_TICK).await;
                continue;
            }
            let (wait, paused_for) = {
                let mut b = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
                let now = Instant::now();
                if call == Call::Add
                    && b.quiet.is_none()
                    && let Some(pauses) = pauses
                {
                    b.quiet = Some(Bucket::new(quiet_limits(pauses)));
                }
                b.refill(now);
                let held = b
                    .adds_held_until
                    .filter(|until| call == Call::Add && *until > now)
                    .map(|until| until - now);
                match b.paused_until.filter(|until| *until > now) {
                    Some(until) => (until - now, held.unwrap_or_default()),
                    None if held.is_some() => (held.unwrap_or_default(), held.unwrap_or_default()),
                    None => {
                        let quiet = b
                            .quiet
                            .as_ref()
                            .filter(|_| call == Call::Add && !user)
                            .map_or(Duration::ZERO, |q| q.shortfall(lane));
                        let wait = b
                            .calls
                            .shortfall(lane)
                            .max(
                                b.call_bucket(call)
                                    .map_or(Duration::ZERO, |c| c.shortfall(lane)),
                            )
                            .max(quiet);
                        if wait.is_zero() {
                            b.calls.tokens -= 1.0;
                            if let Some(bucket) = b.call_bucket(call) {
                                bucket.tokens -= 1.0;
                            }
                            if call == Call::Add
                                && let Some(quiet) = b.quiet.as_mut()
                            {
                                quiet.tokens = (quiet.tokens - 1.0).max(-quiet.limits.burst);
                            }
                            return Ok(());
                        }
                        (wait, quiet)
                    }
                }
            };
            if lane.priority() == BACKGROUND && paused_for > BACKGROUND_ADD_PATIENCE {
                return Err(DebridError::AddsDeferred);
            }
            tokio::time::sleep(if user { wait } else { wait.min(YIELD_TICK) }).await;
        }
    }

    /// The provider refused an add the way it does while pausing the account's adds, so
    /// none are sent until the pause has likely passed.
    fn hold_adds(&self, hold: Duration) {
        let mut b = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        let until = Instant::now() + hold;
        b.adds_held_until = Some(b.adds_held_until.map_or(until, |held| held.max(until)));
        if let Some(quiet) = b.quiet.as_mut() {
            quiet.tokens = quiet.tokens.min(0.0);
        }
    }

    /// Returns whether the call was rate limited, after pausing every caller on the account.
    fn observe<T>(&self, result: &Result<T, DebridError>) -> bool {
        let mut b = self.buckets.lock().unwrap_or_else(|e| e.into_inner());
        match result {
            Err(DebridError::RateLimited { retry_after }) => {
                let pause = retry_after.unwrap_or(b.backoff);
                warn!(
                    pause_ms = pause.as_millis() as u64,
                    "Debrid rate limited, pausing account"
                );
                b.paused_until = Some(Instant::now() + pause);
                b.backoff = (b.backoff * 2).min(MAX_BACKOFF);
                b.calls.tokens = 0.0;
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
    name_trusted: bool,
}

impl Throttled {
    pub fn new(inner: Arc<dyn DebridProvider>, api_key: &str, lane: Lane) -> Self {
        let gate = gate(inner.provider_type(), api_key);
        Self {
            inner,
            gate,
            lane,
            name_trusted: false,
        }
    }

    /// A refusal is read as a pause even when the name is one the provider always refuses:
    /// the release is known to have been taken recently. A second refusal still counts.
    pub fn trusting_name(mut self) -> Self {
        self.name_trusted = true;
        self
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
        self.run_as(Call::Other, call).await
    }

    async fn run_as<'a, T, F, Fut>(&'a self, kind: Call, call: F) -> Result<T, DebridError>
    where
        F: Fn() -> Fut,
        Fut: Future<Output = Result<T, DebridError>> + 'a,
    {
        let started = Instant::now();
        let mut made = 0;
        let pauses = self.inner.add_pauses();
        loop {
            self.gate.acquire(&self.lane, kind, pauses.as_ref()).await?;
            let result = call().await;
            made += 1;
            if !self.gate.observe(&result) {
                return result;
            }
            let more = match self.lane.priority() {
                USER => started.elapsed() < USER_RETRY_WINDOW,
                DOWNLOAD => made < DOWNLOAD_ATTEMPTS,
                _ => made < BACKGROUND_ATTEMPTS,
            };
            if !more {
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
        let add = || {
            self.run_as(Call::Add, || {
                self.inner.add_magnet(http_client, api_key, magnet)
            })
        };
        let result = add().await;
        match (&result, self.inner.add_pauses()) {
            (Err(DebridError::Blocked), Some(pauses))
                if self.name_trusted
                    || !magnet::display_name(magnet).is_some_and(|n| (pauses.refuses_name)(&n)) =>
            {
                warn!(
                    hold_ms = pauses.hold.as_millis() as u64,
                    "Debrid refused an add with a name it takes; holding the account's adds"
                );
                self.gate.hold_adds(pauses.hold);
                add().await
            }
            _ => result,
        }
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
        self.run_as(Call::Unrestrict, || {
            self.inner.unrestrict_link(http_client, api_key, link)
        })
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
            while tokio::time::timeout(
                Duration::from_millis(5),
                gate.acquire(&background, Call::Other, None),
            )
            .await
            .is_ok()
            {
                n += 1;
            }
            n
        }
        .await;
        assert_eq!(taken, 8 - 5);
        let started = Instant::now();
        for _ in 0..2 {
            gate.acquire(&Lane::download(), Call::Other, None)
                .await
                .unwrap();
        }
        for _ in 0..3 {
            gate.acquire(&Lane::user(), Call::Other, None)
                .await
                .unwrap();
        }
        assert!(started.elapsed() < Duration::from_millis(20));
    }

    async fn instant(gate: &Gate, lane: &Lane, call: Call) -> bool {
        instant_with(gate, lane, call, None).await == Some(true)
    }

    /// `Some(true)` granted at once, `Some(false)` refused at once, `None` still waiting.
    async fn instant_with(
        gate: &Gate,
        lane: &Lane,
        call: Call,
        pauses: Option<&AddPauses>,
    ) -> Option<bool> {
        tokio::time::timeout(Duration::from_millis(5), gate.acquire(lane, call, pauses))
            .await
            .ok()
            .map(|r| r.is_ok())
    }

    const PAUSES: AddPauses = AddPauses {
        hold: Duration::from_millis(100),
        burst: 4,
        spacing: Duration::from_secs(45),
        refuses_name: |_| false,
    };

    #[tokio::test]
    async fn warming_adds_only_on_a_quiet_account_and_a_user_never_waits_for_one() {
        let gate = fresh_gate();
        let background = Lane::background();
        let p = Some(&PAUSES);
        assert_eq!(
            instant_with(&gate, &background, Call::Add, p).await,
            Some(true)
        );
        assert_eq!(
            instant_with(&gate, &background, Call::Add, p).await,
            Some(false)
        );
        assert_eq!(
            instant_with(&gate, &Lane::download(), Call::Add, p).await,
            Some(true)
        );
        assert_eq!(
            instant_with(&gate, &Lane::download(), Call::Add, p).await,
            Some(true)
        );
        assert_eq!(
            instant_with(&gate, &Lane::download(), Call::Add, p).await,
            None
        );
        assert_eq!(
            instant_with(&gate, &Lane::user(), Call::Add, p).await,
            Some(true)
        );
        assert_eq!(
            instant_with(&gate, &Lane::user(), Call::Add, p).await,
            Some(true)
        );
    }

    #[tokio::test]
    async fn a_held_account_waits_out_the_hold_before_any_add() {
        let gate = fresh_gate();
        gate.hold_adds(PAUSES.hold);
        let p = Some(&PAUSES);
        assert_eq!(
            instant_with(&gate, &Lane::background(), Call::Add, p).await,
            None
        );
        assert_eq!(instant_with(&gate, &Lane::user(), Call::Add, p).await, None);
        assert_eq!(
            instant_with(&gate, &Lane::user(), Call::Other, p).await,
            Some(true)
        );
        let started = Instant::now();
        gate.acquire(&Lane::user(), Call::Add, p).await.unwrap();
        assert!(started.elapsed() >= Duration::from_millis(80));
    }

    #[tokio::test]
    async fn warming_gives_up_rather_than_wait_out_a_long_hold() {
        let gate = fresh_gate();
        gate.hold_adds(Duration::from_secs(30));
        let started = Instant::now();
        let r = gate
            .acquire(&Lane::background(), Call::Add, Some(&PAUSES))
            .await;
        assert!(matches!(r, Err(DebridError::AddsDeferred)), "{r:?}");
        assert!(started.elapsed() < Duration::from_millis(100));
    }

    #[tokio::test]
    async fn unrestricts_trickle_while_other_calls_flow() {
        let gate = fresh_gate();
        let user = Lane::user();
        for _ in 0..4 {
            assert!(instant(&gate, &user, Call::Unrestrict).await);
        }
        assert!(!instant(&gate, &user, Call::Unrestrict).await);
        assert!(instant(&gate, &user, Call::Other).await);
    }

    #[tokio::test]
    async fn adds_trickle_while_other_calls_flow() {
        let gate = fresh_gate();
        let user = Lane::user();
        for _ in 0..5 {
            assert!(instant(&gate, &user, Call::Add).await);
        }
        assert!(!instant(&gate, &user, Call::Add).await);
        assert!(instant(&gate, &user, Call::Other).await);
    }

    #[tokio::test]
    async fn background_adds_leave_room_for_a_user() {
        let gate = fresh_gate();
        let background = Lane::background();
        let mut taken = 0;
        while instant(&gate, &background, Call::Add).await {
            taken += 1;
        }
        assert_eq!(taken, 2);
        assert!(instant(&gate, &Lane::download(), Call::Add).await);
        assert!(instant(&gate, &Lane::user(), Call::Add).await);
        assert!(instant(&gate, &Lane::user(), Call::Add).await);
    }

    #[tokio::test]
    async fn rate_limit_pauses_every_caller_for_retry_after() {
        let gate = fresh_gate();
        let limited: Result<(), DebridError> = Err(DebridError::RateLimited {
            retry_after: Some(Duration::from_millis(200)),
        });
        assert!(gate.observe(&limited));
        let started = Instant::now();
        gate.acquire(&Lane::user(), Call::Other, None)
            .await
            .unwrap();
        assert!(started.elapsed() >= Duration::from_millis(200));
    }
}
