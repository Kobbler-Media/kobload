use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use axum::http::StatusCode;
use kob_usenet::vendor::nntp;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadFailure {
    MissingArticles,
    CorruptArticles,
    Timeout,
    ProvidersUnavailable,
}

impl ReadFailure {
    pub const ALL: [Self; 4] = [
        Self::MissingArticles,
        Self::CorruptArticles,
        Self::Timeout,
        Self::ProvidersUnavailable,
    ];

    pub const fn code(self) -> &'static str {
        match self {
            Self::MissingArticles => "missing_articles",
            Self::CorruptArticles => "corrupt_articles",
            Self::Timeout => "timeout",
            Self::ProvidersUnavailable => "providers_unavailable",
        }
    }

    pub const fn status(self) -> StatusCode {
        match self {
            Self::MissingArticles | Self::CorruptArticles => StatusCode::BAD_GATEWAY,
            Self::Timeout => StatusCode::GATEWAY_TIMEOUT,
            Self::ProvidersUnavailable => StatusCode::SERVICE_UNAVAILABLE,
        }
    }

    pub const fn is_damage(self) -> bool {
        matches!(self, Self::MissingArticles | Self::CorruptArticles)
    }

    pub fn of(error: &io::Error) -> Self {
        let nntp = error.get_ref().and_then(|inner| {
            inner
                .downcast_ref::<Arc<nntp::Error>>()
                .map(AsRef::as_ref)
                .or_else(|| inner.downcast_ref::<nntp::Error>())
        });
        match nntp {
            Some(nntp::Error::NotFound) => return Self::MissingArticles,
            Some(nntp::Error::CrcMismatch { .. } | nntp::Error::Yenc(_)) => {
                return Self::CorruptArticles;
            }
            Some(_) => return Self::ProvidersUnavailable,
            None => {}
        }
        match error.kind() {
            io::ErrorKind::TimedOut => Self::Timeout,
            io::ErrorKind::InvalidData => Self::CorruptArticles,
            _ if error.to_string().contains("unavailable") => Self::MissingArticles,
            _ => Self::ProvidersUnavailable,
        }
    }
}

#[derive(Default)]
pub struct Damage {
    aborts: [AtomicU64; ReadFailure::ALL.len()],
}

impl Damage {
    pub fn record(&self, failure: ReadFailure) {
        self.aborts[failure as usize].fetch_add(1, Ordering::Relaxed);
    }

    pub fn count(&self, failure: ReadFailure) -> u64 {
        self.aborts[failure as usize].load(Ordering::Relaxed)
    }
}
