use std::sync::LazyLock;

use serde::Serialize;
use sqlx::PgPool;
use tokio::sync::broadcast;
use tracing::warn;
use uuid::Uuid;

use super::flight::Acquired;
use crate::debrid::DebridProviderType;
use crate::repositories::stream_attempts::{self as repo, Change, NewAttempt, StreamAttempt};
use crate::repositories::{app_settings, hash_index};

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AttemptStreamEvent {
    Snapshot { attempts: Vec<StreamAttempt> },
    Created { attempt: StreamAttempt },
    Updated { attempt: StreamAttempt },
}

// Process-wide like the flight and warm-cache maps: attempts are recorded from flights and
// warm probes that outlive the request which started them.
static HUB: LazyLock<broadcast::Sender<AttemptStreamEvent>> =
    LazyLock::new(|| broadcast::channel(256).0);

pub fn subscribe() -> broadcast::Receiver<AttemptStreamEvent> {
    HUB.subscribe()
}

#[derive(Debug, Clone, Copy)]
pub enum Origin {
    Play,
    Warm,
}

impl Origin {
    fn as_str(self) -> &'static str {
        match self {
            Self::Play => "play",
            Self::Warm => "warm",
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum Step {
    Requested,
    WarmHit,
    Joined,
    Reused,
    Added,
    Readded,
    Selected,
    CacheCheck,
    Ready,
    Caching,
    Released,
    Refused,
    Failed,
    TimedOut,
    PollError,
    Played,
    PlaybackFailed,
}

impl Step {
    fn as_str(self) -> &'static str {
        match self {
            Self::Requested => "requested",
            Self::WarmHit => "warm_hit",
            Self::Joined => "joined",
            Self::Reused => "reused",
            Self::Added => "added",
            Self::Readded => "readded",
            Self::Selected => "selected",
            Self::CacheCheck => "cache_check",
            Self::Ready => "ready",
            Self::Caching => "caching",
            Self::Released => "released",
            Self::Refused => "refused",
            Self::Failed => "failed",
            Self::TimedOut => "timed_out",
            Self::PollError => "poll_error",
            Self::Played => "played",
            Self::PlaybackFailed => "playback_failed",
        }
    }
}

pub struct Started<'a> {
    pub provider_id: Uuid,
    pub origin: Origin,
    pub info_hash: &'a str,
    pub file_id: Option<i32>,
    pub release_title: Option<&'a str>,
    pub file_name: Option<&'a str>,
}

#[derive(Debug, Clone, Default)]
pub struct AttemptLog(Option<Handle>);

#[derive(Debug, Clone)]
struct Handle {
    pool: PgPool,
    id: Uuid,
}

impl AttemptLog {
    pub fn off() -> Self {
        Self(None)
    }

    pub async fn start(pool: &PgPool, started: Started<'_>) -> Self {
        if !app_settings::get_debrid_logging_enabled(pool)
            .await
            .unwrap_or(false)
        {
            return Self::off();
        }
        let file_name = match (started.file_name, started.file_id) {
            (Some(name), _) => Some(name.to_string()),
            (None, Some(idx)) => hash_index::get_file_name(pool, started.info_hash, idx)
                .await
                .ok()
                .flatten(),
            (None, None) => None,
        };
        // A pack's hash-level title names the pack, or one episode of it at best.
        let title = match (started.release_title, &file_name) {
            (Some(t), _) => Some(t.to_string()),
            (None, Some(name)) => Some(super::service::strip_file_extension(name)),
            (None, None) => hash_index::get_original_title(pool, started.info_hash)
                .await
                .ok()
                .flatten(),
        };
        let new = NewAttempt {
            provider_id: started.provider_id,
            origin: started.origin.as_str(),
            info_hash: started.info_hash,
            file_id: started.file_id,
            release_title: title.as_deref(),
            file_name: file_name.as_deref(),
        };
        let id = match repo::insert(pool, &new).await {
            Ok(id) => id,
            Err(e) => {
                warn!(error = %e, "Failed to record stream attempt");
                return Self::off();
            }
        };
        let handle = Handle {
            pool: pool.clone(),
            id,
        };
        let message = match started.origin {
            Origin::Play => "Client asked to play this release",
            Origin::Warm => "Probing the provider ahead of playback",
        };
        handle
            .write(Some((Step::Requested, message)), &Change::default())
            .await;
        if let Some(attempt) = handle.read().await {
            let _ = HUB.send(AttemptStreamEvent::Created { attempt });
        }
        Self(Some(handle))
    }

    pub async fn for_torrent(pool: &PgPool, provider_id: Uuid, torrent_id: &str) -> Vec<Self> {
        repo::caching_for_torrent(pool, provider_id, torrent_id)
            .await
            .unwrap_or_default()
            .into_iter()
            .map(|id| Self::attach(pool, id))
            .collect()
    }

    pub async fn for_release(
        pool: &PgPool,
        provider_id: Option<Uuid>,
        info_hash: &str,
        file_id: Option<i32>,
    ) -> Self {
        match repo::latest_for_release(pool, provider_id, info_hash, file_id).await {
            Ok(Some(id)) => Self::attach(pool, id),
            _ => Self::off(),
        }
    }

    fn attach(pool: &PgPool, id: Uuid) -> Self {
        Self(Some(Handle {
            pool: pool.clone(),
            id,
        }))
    }

    pub fn id(&self) -> Option<Uuid> {
        self.0.as_ref().map(|h| h.id)
    }

    pub async fn record(&self, step: Step, message: impl AsRef<str>, change: Change) {
        if let Some(h) = &self.0 {
            h.publish(Some((step, message.as_ref())), &change).await;
        }
    }

    pub async fn progress(&self, progress: f32) {
        if let Some(h) = &self.0 {
            let change = Change {
                progress: Some(progress),
                ..Default::default()
            };
            h.publish(None, &change).await;
        }
    }

    pub async fn finish(&self, out: &Acquired) {
        let (step, message, change) = match out {
            Acquired::Ready { link, .. } => (
                Step::Ready,
                format!("Stream ready: {}", link.filename),
                Change {
                    status: Some("ready"),
                    progress: Some(1.0),
                    ..Default::default()
                },
            ),
            Acquired::Pending { torrent_id, .. } => (
                Step::Caching,
                format!(
                    "Not cached yet; waiting for the provider to download torrent {torrent_id}"
                ),
                Change {
                    status: Some("caching"),
                    torrent_id: Some(raw_torrent_id(torrent_id)),
                    ..Default::default()
                },
            ),
            Acquired::Refused => (
                Step::Refused,
                super::flight::REFUSED_MESSAGE.to_string(),
                Change {
                    status: Some("refused"),
                    error: Some(super::flight::REFUSED_MESSAGE.to_string()),
                    ..Default::default()
                },
            ),
            Acquired::Failed { message } => {
                (Step::Failed, message.clone(), Change::failed(message))
            }
        };
        self.record(step, message, change).await;
    }
}

pub fn raw_torrent_id(id: &str) -> String {
    DebridProviderType::from_prefixed_id(id)
        .map(|(_, raw)| raw.to_string())
        .unwrap_or_else(|| id.to_string())
}

impl Handle {
    async fn write(&self, event: Option<(Step, &str)>, change: &Change) {
        if let Err(e) = repo::update(&self.pool, self.id, change).await {
            warn!(attempt = %self.id, error = %e, "Failed to update stream attempt");
        }
        if let Some((step, message)) = event
            && let Err(e) = repo::add_event(&self.pool, self.id, step.as_str(), message).await
        {
            warn!(attempt = %self.id, error = %e, "Failed to record stream attempt event");
        }
    }

    async fn read(&self) -> Option<StreamAttempt> {
        repo::get(&self.pool, self.id).await.ok().flatten()
    }

    async fn publish(&self, event: Option<(Step, &str)>, change: &Change) {
        self.write(event, change).await;
        if HUB.receiver_count() > 0
            && let Some(attempt) = self.read().await
        {
            let _ = HUB.send(AttemptStreamEvent::Updated { attempt });
        }
    }
}
