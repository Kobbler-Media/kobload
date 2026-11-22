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
    let engine =
        Arc::new(Engine::new(EngineConfig::new(settings.data_dir.clone(), Vec::new())).await?);
    tokio::spawn(
        ConfigPull::new(
            Arc::clone(&engine),
            &settings.kobload_url,
            &settings.control_key,
        )
        .run(PULL_EVERY),
    );
    let app = router(AppState::new(
        engine,
        &settings.control_key,
        settings.stream_secret.as_bytes(),
    ));
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
