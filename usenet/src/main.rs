use std::net::{Ipv4Addr, SocketAddr};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use kob_usenet::EngineConfig;
use kobload_usenet::{
    AppState, Booted, ConfigPull, Settings, init_logging, router, start_engine, watch_providers,
};

const PULL_EVERY: Duration = Duration::from_secs(60);
const WATCH_EVERY: Duration = Duration::from_secs(30);

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
    config.download.free_space_floor = settings.free_space_floor;
    config.download.active_jobs = settings.active_jobs;
    let Booted { engine, restored } = start_engine(config).await?;
    let state = AppState::new(
        Arc::clone(&engine),
        &settings.control_key,
        settings.stream_secret.as_bytes(),
    )
    .with_metrics_key(settings.metrics_key.as_deref())
    .with_download_dir(&settings.download_dir);
    let mut pull = ConfigPull::new(
        Arc::clone(&engine),
        &settings.kobload_url,
        &settings.control_key,
        &settings.data_dir,
        state.configured(),
    )
    .with_refresh(state.config_refresh())
    .with_hosts(state.provider_hosts());
    if let Some(servers) = restored {
        pull.adopt(servers);
    }
    tokio::spawn(pull.run(PULL_EVERY));
    tokio::spawn(watch_providers(engine, WATCH_EVERY));
    let app = router(state);
    let addr = SocketAddr::from((Ipv4Addr::UNSPECIFIED, settings.port));
    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(
        %addr,
        data_dir = %settings.data_dir.display(),
        download_dir = %settings.download_dir.display(),
        kobload = settings.kobload_url,
        "serving"
    );
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
