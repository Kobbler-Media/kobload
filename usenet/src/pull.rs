use std::sync::Arc;
use std::time::Duration;

use kob_usenet::Engine;
use reqwest::header::AUTHORIZATION;
use usenet_proto::{EngineConfigResponse, UsenetServer};

use crate::servers::engine_server;

const TIMEOUT: Duration = Duration::from_secs(15);

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
}

pub struct ConfigPull {
    engine: Arc<Engine>,
    http: reqwest::Client,
    url: String,
    bearer: String,
    applied: Option<Vec<UsenetServer>>,
}

impl ConfigPull {
    #[must_use]
    pub fn new(engine: Arc<Engine>, kobload_url: &str, control_key: &str) -> Self {
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
            applied: None,
        }
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
        self.engine
            .reconfigure(config.servers.iter().cloned().map(engine_server).collect())?;
        for server in &config.servers {
            tracing::info!(
                name = server.name,
                host = server.host,
                port = server.port,
                priority = server.priority,
                connections = server.connections,
                "usenet server applied"
            );
        }
        self.applied = Some(config.servers);
        Ok(Pulled::Applied(count))
    }

    pub async fn run(mut self, every: Duration) {
        let mut ticks = tokio::time::interval(every);
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            ticks.tick().await;
            match self.tick().await {
                Ok(Pulled::Applied(count)) => {
                    tracing::info!(servers = count, "engine config applied")
                }
                Ok(Pulled::Unchanged) => {}
                Err(error) => tracing::warn!(%error, "engine config pull failed"),
            }
        }
    }
}
