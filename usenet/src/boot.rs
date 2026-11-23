use std::sync::Arc;

use kob_usenet::{Engine, EngineConfig};
use usenet_proto::UsenetServer;

use crate::pull::stored_servers;
use crate::servers::engine_server;

pub struct Booted {
    pub engine: Arc<Engine>,
    pub restored: Option<Vec<UsenetServer>>,
}

// `Engine::new` resumes unfinished jobs at once, so the stored servers go into its config
// rather than being applied after: a job started without servers burns a retry attempt.
pub async fn start_engine(mut config: EngineConfig) -> Result<Booted, kob_usenet::Error> {
    let restored = match stored_servers(&config.data_dir) {
        Ok(restored) => restored,
        Err(error) => {
            tracing::warn!(%error, "could not restore the last engine config");
            None
        }
    };
    config.servers = restored
        .iter()
        .flatten()
        .cloned()
        .map(engine_server)
        .collect();
    let engine = Arc::new(Engine::new(config).await?);
    if restored.is_some() {
        tracing::info!("serving the last applied engine config until kobload answers");
        let warm = Arc::clone(&engine);
        tokio::spawn(async move { warm.prewarm().await });
    }
    Ok(Booted { engine, restored })
}
