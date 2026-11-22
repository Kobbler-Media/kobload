use std::fmt::Write as _;

use axum::extract::State;
use axum::http::header;
use axum::response::IntoResponse;
use kob_usenet::ProviderStats;
use kob_usenet::vendor::core::trace::Outcome;
use kob_usenet::vendor::nntp::Workload;

use crate::state::AppState;

pub async fn metrics(State(state): State<AppState>) -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "text/plain; version=0.0.4")],
        render(&state.engine.stats()),
    )
}

fn render(providers: &[ProviderStats]) -> String {
    let mut out = String::new();
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
        "Failed connects.",
        |p| p.connect_failures,
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

fn family(out: &mut String, name: &str, kind: &str, help: &str) {
    let _ = writeln!(out, "# HELP {name} {help}\n# TYPE {name} {kind}");
}

fn escape(label: &str) -> String {
    label
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
        .replace('\n', "\\n")
}
