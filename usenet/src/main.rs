use std::net::{Ipv4Addr, SocketAddr};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use kob_usenet::{Engine, EngineConfig};
use kobload_usenet::{AppState, ConfigPull, Settings, init_logging, router};

const PULL_EVERY: Duration = Duration::from_secs(60);

#[tokio::main]
async fn main() -> ExitCode {
    init_logging();
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(%error, "kobload-usenet stopped");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), Box<dyn std::error::Error>> {
    let settings = Settings::from_env()?;
    let mut config = EngineConfig::new(settings.data_dir.clone(), Vec::new());
    config.min_connections = settings.min_connections;
    let engine = Arc::new(Engine::new(config).await?);
    let state = AppState::new(
        Arc::clone(&engine),
        &settings.control_key,
        settings.stream_secret.as_bytes(),
    )
    .with_metrics_key(settings.metrics_key.as_deref());
    let mut pull = ConfigPull::new(
        engine,
        &settings.kobload_url,
        &settings.control_key,
        &settings.data_dir,
        state.configured(),
    );
    match pull.restore() {
        Ok(true) => tracing::info!("serving the last applied engine config until kobload answers"),
        Ok(false) => {}
        Err(error) => tracing::warn!(%error, "could not restore the last engine config"),
    }
    tokio::spawn(pull.run(PULL_EVERY));
    let app = router(state);
    let addr = SocketAddr::from((Ipv4Addr::UNSPECIFIED, settings.port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, data_dir = %settings.data_dir.display(), kobload = settings.kobload_url, "serving");
    tokio::select! {
        served = axum::serve(listener, app).into_future() => served?,
        () = terminated() => tracing::info!("shutting down"),
    }
    Ok(())
}

// In-flight streams are cut rather than drained: kobbler resumes them with a Range request,
// and draining a long stream would outlast the container's stop timeout.
async fn terminated() {
    let Ok(mut term) = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
    else {
        return std::future::pending().await;
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = term.recv() => {}
    }
}
