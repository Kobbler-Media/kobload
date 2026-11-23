use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use kob_usenet::Engine;
use tokio::sync::{Notify, Semaphore};

use crate::damage::Damage;
use crate::status::{ProviderHosts, SharedRates};
use crate::stream::LiveStreams;
use crate::throttle::Throttle;

pub const BYTE_GAP: Duration = Duration::from_secs(7);
pub const DEFAULT_DOWNLOAD_DIR: &str = "/downloads";
// Each import buffers its NZB about three times over, up to the 128 MB body limit.
const IMPORTS_AT_ONCE: usize = 4;
const REFUSAL_WARN_EVERY: Duration = Duration::from_secs(60);

#[derive(Clone, Default)]
pub struct Configured(Arc<AtomicBool>);

impl Configured {
    pub fn set(&self) {
        self.0.store(true, Ordering::Release);
    }

    #[must_use]
    pub fn is_set(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

#[derive(Clone)]
pub struct AppState {
    pub(crate) engine: Arc<Engine>,
    pub(crate) control_key: Arc<str>,
    pub(crate) metrics_key: Option<Arc<str>>,
    pub(crate) stream_secret: Arc<[u8]>,
    pub(crate) byte_gap: Duration,
    pub(crate) configured: Configured,
    pub(crate) damage: Arc<Damage>,
    pub(crate) download_dir: Arc<Path>,
    pub(crate) refused_tokens: Arc<Throttle>,
    pub(crate) uploads: Arc<Semaphore>,
    pub(crate) config_refresh: Arc<Notify>,
    pub(crate) live: Arc<LiveStreams>,
    pub(crate) rates: Arc<SharedRates>,
    pub(crate) hosts: ProviderHosts,
}

impl AppState {
    #[must_use]
    pub fn new(engine: Arc<Engine>, control_key: &str, stream_secret: &[u8]) -> Self {
        Self {
            engine,
            control_key: control_key.into(),
            metrics_key: None,
            stream_secret: stream_secret.into(),
            byte_gap: BYTE_GAP,
            configured: Configured::default(),
            damage: Arc::default(),
            download_dir: PathBuf::from(DEFAULT_DOWNLOAD_DIR).into(),
            refused_tokens: Arc::new(Throttle::new(REFUSAL_WARN_EVERY)),
            uploads: Arc::new(Semaphore::new(IMPORTS_AT_ONCE)),
            config_refresh: Arc::default(),
            live: Arc::default(),
            rates: Arc::default(),
            hosts: ProviderHosts::default(),
        }
    }

    #[must_use]
    pub fn with_metrics_key(mut self, metrics_key: Option<&str>) -> Self {
        self.metrics_key = metrics_key.map(Into::into);
        self
    }

    #[must_use]
    pub fn with_download_dir(mut self, download_dir: &Path) -> Self {
        self.download_dir = download_dir.into();
        self
    }

    #[must_use]
    pub const fn with_byte_gap(mut self, byte_gap: Duration) -> Self {
        self.byte_gap = byte_gap;
        self
    }

    #[must_use]
    pub fn config_refresh(&self) -> Arc<Notify> {
        Arc::clone(&self.config_refresh)
    }

    #[must_use]
    pub fn provider_hosts(&self) -> ProviderHosts {
        self.hosts.clone()
    }

    #[must_use]
    pub fn configured(&self) -> Configured {
        self.configured.clone()
    }
}
