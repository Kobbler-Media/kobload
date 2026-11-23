mod hosts;
mod rates;
mod view;

use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::State;
use kob_usenet::ProviderStats;
use usenet_proto::{EngineStatus, ProviderActivity, StreamActivity, StreamKind};

use crate::state::AppState;
pub use hosts::ProviderHosts;
pub use rates::RateWindow;
pub use view::{Holds, engine_activity, job_activity, job_phase};

const WINDOW: Duration = Duration::from_secs(5);

pub struct Rates {
    providers: RateWindow<String>,
    jobs: RateWindow<String>,
    streams: RateWindow<u64>,
}

impl Default for Rates {
    fn default() -> Self {
        Self {
            providers: RateWindow::new(WINDOW),
            jobs: RateWindow::new(WINDOW),
            streams: RateWindow::new(WINDOW),
        }
    }
}

pub type SharedRates = Mutex<Rates>;

fn received(provider: &ProviderStats) -> u64 {
    provider.workloads.iter().map(|w| w.bytes).sum()
}

pub async fn status(State(state): State<AppState>) -> Json<EngineStatus> {
    let now = Instant::now();
    let stats = state.engine.stats();
    let jobs = state.engine.jobs();
    let live = state.live.snapshot();
    let low_disk = state.engine.paused_for_space();

    let mut rates = state.rates.lock().unwrap_or_else(PoisonError::into_inner);
    rates
        .providers
        .record(now, stats.iter().map(|p| (p.key.clone(), received(p))));
    rates.jobs.record(
        now,
        jobs.iter()
            .filter(|j| !j.state.is_finished())
            .map(|j| (j.id.clone(), j.bytes_done)),
    );
    rates
        .streams
        .record(now, live.iter().map(|s| (s.id, s.sent)));

    let known: Vec<u64> = stats
        .iter()
        .filter_map(|p| rates.providers.rate(&p.key))
        .collect();
    let speed_bps = (!known.is_empty()).then(|| known.iter().sum());
    let providers: Vec<ProviderActivity> = stats
        .iter()
        .map(|p| ProviderActivity {
            host: state.hosts.host(&p.key),
            in_use: p.active,
            open: p.live,
            max: p.max_connections,
            failing: p.connect_error.is_some(),
        })
        .collect();
    let streams: Vec<StreamActivity> = live
        .iter()
        .map(|s| StreamActivity {
            id: s.id,
            name: s.name.clone(),
            kind: s.kind,
            position_percent: s.position_percent,
            speed_bps: rates.streams.rate(&s.id),
        })
        .collect();
    // Warm reads rank below downloads, so only playback and cache-ahead can hold one.
    let holds = Holds {
        low_disk,
        stream: streams.iter().any(|s| s.kind != StreamKind::Preloading),
    };
    let jobs: Vec<_> = jobs
        .iter()
        .filter_map(|j| job_activity(j, rates.jobs.rate(&j.id), holds))
        .collect();
    drop(rates);

    Json(EngineStatus {
        activity: engine_activity(low_disk, &providers, &jobs, &streams),
        speed_bps,
        providers,
        jobs,
        streams,
    })
}
