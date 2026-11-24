use std::fmt::Write as _;

use kob_usenet::vendor::nntp::{
    HistogramSnapshot, LOAD_BUCKETS, STAGE_BUCKETS, WorkloadStageSnapshot,
};

use crate::metrics::family;

const STAGES: &str = "kobload_usenet_stream_stage_seconds";
const ROUTES: &str = "kobload_usenet_stream_connection_routes_total";
const PATHS: &str = "kobload_usenet_stream_connection_paths_total";
const LOAD: &str = "kobload_usenet_stream_assign_load";
const BYTES: &str = "kobload_usenet_stream_body_bytes_total";
const ABANDONED: &str = "kobload_usenet_stream_abandoned_total";

pub fn render_stages(out: &mut String, workloads: &[WorkloadStageSnapshot]) {
    family(
        out,
        STAGES,
        "histogram",
        "Time per stage of delivered stream and cache-ahead requests. connect, tls and auth sit inside connection_wait; service and service_first_read are per HTTP read.",
    );
    for snapshot in workloads {
        let workload = snapshot.workload.label();
        for (stage, histogram) in &snapshot.stages {
            let labels = format!("workload=\"{workload}\",stage=\"{}\"", stage.label());
            histogram_lines(
                out,
                STAGES,
                &labels,
                &STAGE_BUCKETS,
                histogram,
                histogram.sum_seconds(),
            );
        }
    }
    family(
        out,
        ROUTES,
        "counter",
        "Body attempts by how they got a connection: idle, busy (pipelined) or new.",
    );
    for snapshot in workloads {
        for (route, count) in &snapshot.routes {
            let _ = writeln!(
                out,
                "{ROUTES}{{workload=\"{}\",route=\"{}\"}} {count}",
                snapshot.workload.label(),
                route.label()
            );
        }
    }
    family(
        out,
        PATHS,
        "counter",
        "Body attempts by whether their connection moved body data in the last second (warm) or not (cold).",
    );
    for snapshot in workloads {
        for (path, count) in &snapshot.paths {
            let _ = writeln!(
                out,
                "{PATHS}{{workload=\"{}\",path=\"{}\"}} {count}",
                snapshot.workload.label(),
                path.label()
            );
        }
    }
    family(
        out,
        LOAD,
        "histogram",
        "Pool load when a request took its connection. requests_ahead covers both workloads, the rest playback only.",
    );
    for snapshot in workloads {
        for (measure, histogram) in &snapshot.load {
            let labels = format!(
                "workload=\"{}\",measure=\"{}\"",
                snapshot.workload.label(),
                measure.label()
            );
            histogram_lines(
                out,
                LOAD,
                &labels,
                &LOAD_BUCKETS,
                histogram,
                histogram.sum as f64,
            );
        }
    }
    counter(
        out,
        BYTES,
        "Decoded bytes of delivered requests; divide by body_transfer for throughput.",
        workloads,
        |s| s.body_bytes,
    );
    counter(
        out,
        ABANDONED,
        "Body attempts given a connection but not delivered: cancelled or failed.",
        workloads,
        |s| s.abandoned,
    );
}

fn counter(
    out: &mut String,
    name: &str,
    help: &str,
    workloads: &[WorkloadStageSnapshot],
    value: fn(&WorkloadStageSnapshot) -> u64,
) {
    family(out, name, "counter", help);
    for snapshot in workloads {
        let _ = writeln!(
            out,
            "{name}{{workload=\"{}\"}} {}",
            snapshot.workload.label(),
            value(snapshot)
        );
    }
}

fn histogram_lines<B: std::fmt::Display>(
    out: &mut String,
    name: &str,
    labels: &str,
    bounds: &[B],
    histogram: &HistogramSnapshot,
    sum: f64,
) {
    for (bound, count) in bounds.iter().zip(&histogram.cumulative) {
        let _ = writeln!(out, "{name}_bucket{{{labels},le=\"{bound}\"}} {count}");
    }
    let count = histogram.count();
    let _ = writeln!(out, "{name}_bucket{{{labels},le=\"+Inf\"}} {count}");
    let _ = writeln!(out, "{name}_sum{{{labels}}} {sum}");
    let _ = writeln!(out, "{name}_count{{{labels}}} {count}");
}
