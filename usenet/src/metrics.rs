use std::fmt::Write as _;

use axum::extract::State;
use axum::http::header;
use axum::response::IntoResponse;
use kob_usenet::vendor::core::trace::Outcome;
use kob_usenet::vendor::nntp::Workload;
use kob_usenet::{JobStatus, ProviderStats};

use crate::damage::{Damage, ReadFailure};
use crate::stage_metrics::render_stages;
use crate::state::AppState;

pub async fn metrics(State(state): State<AppState>) -> impl IntoResponse {
    ([(header::CONTENT_TYPE, "text/plain; version=0.0.4")], {
        let mut out = render(
            &state.engine.stats(),
            &state.damage,
            &state.engine.jobs(),
            state.engine.paused_for_space(),
        );
        render_stages(&mut out, &state.engine.stream_stages().snapshot());
        out
    })
}

const JOB_STATES: [(kob_usenet::JobState, &str); 7] = {
    use kob_usenet::JobState as S;
    [
        (S::Queued, "queued"),
        (S::Downloading, "downloading"),
        (S::Verifying, "verifying"),
        (S::Repairing, "repairing"),
        (S::Unpacking, "unpacking"),
        (S::Completed, "completed"),
        (S::Failed, "failed"),
    ]
};

fn render_jobs(out: &mut String, jobs: &[JobStatus], paused_for_space: bool) {
    family(
        out,
        "kobload_usenet_jobs",
        "gauge",
        "Download jobs by state.",
    );
    for (state, label) in JOB_STATES {
        let count = jobs.iter().filter(|j| j.state == state).count();
        let _ = writeln!(out, "kobload_usenet_jobs{{state=\"{label}\"}} {count}");
    }
    family(
        out,
        "kobload_usenet_paused_for_space",
        "gauge",
        "1 while downloads wait for free space above the floor.",
    );
    let _ = writeln!(
        out,
        "kobload_usenet_paused_for_space {}",
        u8::from(paused_for_space)
    );
}

fn render(
    providers: &[ProviderStats],
    damage: &Damage,
    jobs: &[JobStatus],
    paused_for_space: bool,
) -> String {
    let mut out = String::new();
    render_jobs(&mut out, jobs, paused_for_space);
    family(
        &mut out,
        "kobload_usenet_stream_failures_total",
        "counter",
        "Stream reads refused or aborted, by reason.",
    );
    for failure in ReadFailure::ALL {
        let _ = writeln!(
            out,
            "kobload_usenet_stream_failures_total{{code=\"{}\"}} {}",
            failure.code(),
            damage.count(failure)
        );
    }
    let mut per_provider =
        |name: &str, kind: &str, help: &str, value: fn(&ProviderStats) -> u64| {
            family(&mut out, name, kind, help);
            for p in providers {
                let _ = writeln!(
                    out,
                    "{name}{{provider=\"{}\"}} {}",
                    escape(&p.name),
                    value(p)
                );
            }
        };
    per_provider(
        "kobload_usenet_provider_connections",
        "gauge",
        "Open NNTP connections.",
        |p| p.live as u64,
    );
    per_provider(
        "kobload_usenet_provider_busy",
        "gauge",
        "Requests in flight or queued on a connection.",
        |p| p.busy as u64,
    );
    per_provider(
        "kobload_usenet_connect_failures_total",
        "counter",
        "Failed connects, logins refused by the provider included.",
        |p| p.connect_failures,
    );
    per_provider(
        "kobload_usenet_provider_failing",
        "gauge",
        "1 while connects or logins fail with no connection open.",
        |p| u64::from(p.connect_error.is_some()),
    );
    per_provider(
        "kobload_usenet_keep_warm_stats_total",
        "counter",
        "STATs sent only to keep an idle connection's congestion window open while someone watches.",
        |p| p.keep_warm_stats,
    );
    per_provider(
        "kobload_usenet_provider_warm_connections",
        "gauge",
        "Open connections that moved a body or a keep-warm reply within the last second.",
        |p| p.warm_connections as u64,
    );
    per_provider(
        "kobload_usenet_provider_cold_connections",
        "gauge",
        "Open connections that have gone idle long enough to restart slow start.",
        |p| p.connections.len().saturating_sub(p.warm_connections) as u64,
    );
    per_provider(
        "kobload_usenet_breaker_trips_total",
        "counter",
        "Times the circuit breaker opened.",
        |p| p.trips,
    );

    family(
        &mut out,
        "kobload_usenet_body_attempts_total",
        "counter",
        "BODY attempts by workload and outcome.",
    );
    for p in providers {
        for workload in Workload::ALL {
            let stats = &p.workloads[workload as usize];
            for outcome in Outcome::ALL {
                let _ = writeln!(
                    out,
                    "kobload_usenet_body_attempts_total{{provider=\"{}\",workload=\"{}\",outcome=\"{}\"}} {}",
                    escape(&p.name),
                    workload.label(),
                    outcome.label(),
                    stats.attempts[outcome.index()]
                );
            }
        }
    }

    family(
        &mut out,
        "kobload_usenet_body_bytes_total",
        "counter",
        "Decoded BODY bytes by workload.",
    );
    for p in providers {
        for workload in Workload::ALL {
            let _ = writeln!(
                out,
                "kobload_usenet_body_bytes_total{{provider=\"{}\",workload=\"{}\"}} {}",
                escape(&p.name),
                workload.label(),
                p.workloads[workload as usize].bytes
            );
        }
    }
    out
}

pub(crate) fn family(out: &mut String, name: &str, kind: &str, help: &str) {
    let _ = writeln!(out, "# HELP {name} {help}\n# TYPE {name} {kind}");
}

fn escape(label: &str) -> String {
    label
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}
