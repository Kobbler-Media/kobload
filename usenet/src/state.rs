use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use kob_usenet::Engine;

use crate::damage::Damage;

pub const BYTE_GAP: Duration = Duration::from_secs(7);

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
        }
    }

    #[must_use]
    pub fn with_metrics_key(mut self, metrics_key: Option<&str>) -> Self {
        self.metrics_key = metrics_key.map(Into::into);
        self
    }

    #[must_use]
    pub const fn with_byte_gap(mut self, byte_gap: Duration) -> Self {
        self.byte_gap = byte_gap;
        self
    }

    #[must_use]
    pub fn configured(&self) -> Configured {
        self.configured.clone()
    }
}
