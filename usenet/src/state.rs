use std::sync::Arc;
use std::time::Duration;

use kob_usenet::Engine;

pub const BYTE_GAP: Duration = Duration::from_secs(7);

#[derive(Clone)]
pub struct AppState {
    pub(crate) engine: Arc<Engine>,
    pub(crate) control_key: Arc<str>,
    pub(crate) stream_secret: Arc<[u8]>,
    pub(crate) byte_gap: Duration,
}

impl AppState {
    #[must_use]
    pub fn new(engine: Arc<Engine>, control_key: &str, stream_secret: &[u8]) -> Self {
        Self {
            engine,
            control_key: control_key.into(),
            stream_secret: stream_secret.into(),
            byte_gap: BYTE_GAP,
        }
    }

    #[must_use]
    pub const fn with_byte_gap(mut self, byte_gap: Duration) -> Self {
        self.byte_gap = byte_gap;
        self
    }
}
