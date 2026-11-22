use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use kob_usenet::Engine;
use reqwest::header::AUTHORIZATION;
use usenet_proto::{EngineConfigResponse, UsenetServer};

use crate::servers::engine_server;
use crate::state::Configured;

const TIMEOUT: Duration = Duration::from_secs(15);
const FIRST_RETRY: Duration = Duration::from_secs(1);
const MAX_RETRY: Duration = Duration::from_secs(2);
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
                    tracing::warn!(%error, retry_in = ?wait, "first engine config pull failed");
                    tokio::time::sleep(wait).await;
                    wait = (wait * 2).min(MAX_RETRY);
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
                Err(error) => tracing::warn!(%error, "engine config pull failed"),
            }
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
        file.write_all(&body)?;
        file.sync_all()?;
        std::fs::rename(&partial, &self.stored)?;
        Ok(())
    }
}
