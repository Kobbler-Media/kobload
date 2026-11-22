use std::io::Write as _;
use std::os::unix::fs::{OpenOptionsExt as _, PermissionsExt as _};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use kob_usenet::Engine;
use reqwest::header::AUTHORIZATION;
use usenet_proto::{EngineConfigResponse, UsenetServer};

use crate::servers::engine_server;
use crate::state::Configured;

const TIMEOUT: Duration = Duration::from_secs(15);
const FIRST_RETRY: Duration = Duration::from_secs(1);
const MAX_RETRY: Duration = Duration::from_secs(30);
const WARN_EVERY: Duration = Duration::from_secs(60);
const STORED: &str = "config.json";

#[derive(Debug, PartialEq, Eq)]
pub enum Pulled {
    Applied(usize),
    Unchanged,
}

#[derive(Debug, thiserror::Error)]
pub enum PullError {
    #[error("fetching engine config: {0}")]
    Fetch(#[from] reqwest::Error),
    #[error("applying engine config: {0}")]
    Apply(#[from] kob_usenet::Error),
    #[error("storing engine config: {0}")]
    Store(#[from] std::io::Error),
    #[error("stored engine config is unreadable: {0}")]
    Stored(#[from] serde_json::Error),
}

pub struct ConfigPull {
    engine: Arc<Engine>,
    http: reqwest::Client,
    url: String,
    bearer: String,
    stored: PathBuf,
    configured: Configured,
    applied: Option<Vec<UsenetServer>>,
    warned: WarnGate,
}

#[derive(Default)]
struct WarnGate(Option<Instant>);

impl WarnGate {
    fn due(&mut self, now: Instant) -> bool {
        let due = self
            .0
            .is_none_or(|last| now.duration_since(last) >= WARN_EVERY);
        if due {
            self.0 = Some(now);
        }
        due
    }
}

fn backoff(wait: Duration) -> Duration {
    (wait * 2).min(MAX_RETRY)
}

impl ConfigPull {
    #[must_use]
    pub fn new(
        engine: Arc<Engine>,
        kobload_url: &str,
        control_key: &str,
        data_dir: &Path,
        configured: Configured,
    ) -> Self {
        Self {
            engine,
            http: reqwest::Client::builder()
                .timeout(TIMEOUT)
                .build()
                .unwrap_or_default(),
            url: format!(
                "{}/api/v1/usenet/engine-config",
                kobload_url.trim_end_matches('/')
            ),
            bearer: format!("Bearer {control_key}"),
            stored: data_dir.join(STORED),
            configured,
            applied: None,
            warned: WarnGate::default(),
        }
    }

    // kobload may be down or redeploying when the engine starts; the last config it applied
    // lets streams resume at once instead of waiting for the first pull.
    pub fn restore(&mut self) -> Result<bool, PullError> {
        let raw = match std::fs::read(&self.stored) {
            Ok(raw) => raw,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(e) => return Err(e.into()),
        };
        let config: EngineConfigResponse = serde_json::from_slice(&raw)?;
        self.apply(config.servers, false)?;
        Ok(true)
    }

    pub async fn tick(&mut self) -> Result<Pulled, PullError> {
        let config: EngineConfigResponse = self
            .http
            .get(&self.url)
            .header(AUTHORIZATION, &self.bearer)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        if self.applied.as_ref() == Some(&config.servers) {
            return Ok(Pulled::Unchanged);
        }
        let count = config.servers.len();
        self.apply(config.servers, true)?;
        Ok(Pulled::Applied(count))
    }

    pub async fn first(&mut self) -> Pulled {
        let mut wait = FIRST_RETRY;
        loop {
            match self.tick().await {
                Ok(pulled) => return pulled,
                Err(error) => {
                    self.failed(&error, "first engine config pull failed");
                    tokio::time::sleep(wait).await;
                    wait = backoff(wait);
                }
            }
        }
    }

    pub async fn run(mut self, every: Duration) {
        if let Pulled::Applied(count) = self.first().await {
            tracing::info!(servers = count, "engine config applied");
        }
        let mut ticks = tokio::time::interval(every);
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        ticks.tick().await;
        loop {
            ticks.tick().await;
            match self.tick().await {
                Ok(Pulled::Applied(count)) => {
                    tracing::info!(servers = count, "engine config applied");
                }
                Ok(Pulled::Unchanged) => {}
                Err(error) => self.failed(&error, "engine config pull failed"),
            }
        }
    }

    fn failed(&mut self, error: &PullError, message: &str) {
        if self.warned.due(Instant::now()) {
            tracing::warn!(%error, "{message}");
        } else {
            tracing::debug!(%error, "{message}");
        }
    }

    fn apply(&mut self, servers: Vec<UsenetServer>, persist: bool) -> Result<(), PullError> {
        self.engine
            .reconfigure(servers.iter().cloned().map(engine_server).collect())?;
        for server in &servers {
            tracing::info!(
                name = server.name,
                host = server.host,
                port = server.port,
                priority = server.priority,
                connections = server.connections,
                "usenet server applied"
            );
        }
        if persist {
            self.persist(&servers)?;
        }
        self.configured.set();
        let engine = Arc::clone(&self.engine);
        tokio::spawn(async move { engine.prewarm().await });
        self.applied = Some(servers);
        Ok(())
    }

    // The file holds provider passwords, so it is created owner-only and swapped in whole.
    fn persist(&self, servers: &[UsenetServer]) -> Result<(), PullError> {
        let body = serde_json::to_vec(&EngineConfigResponse {
            servers: servers.to_vec(),
        })?;
        let partial = self.stored.with_extension("json.partial");
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&partial)?;
        // `mode` applies only to a new file; a partial left by a crash keeps its own.
        file.set_permissions(std::fs::Permissions::from_mode(0o600))?;
        file.write_all(&body)?;
        file.sync_all()?;
        std::fs::rename(&partial, &self.stored)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retries_back_off_to_half_a_minute() {
        let mut wait = FIRST_RETRY;
        let mut waits = Vec::new();
        for _ in 0..7 {
            waits.push(wait.as_secs());
            wait = backoff(wait);
        }
        assert_eq!(waits, [1, 2, 4, 8, 16, 30, 30]);
    }

    #[test]
    fn failures_warn_at_most_once_a_minute() {
        let start = Instant::now();
        let mut gate = WarnGate::default();
        let due: Vec<bool> = [0, 1, 30, 59, 60, 61, 120]
            .into_iter()
            .map(|s| gate.due(start + Duration::from_secs(s)))
            .collect();
        assert_eq!(due, [true, false, false, false, true, false, true]);
    }
}
