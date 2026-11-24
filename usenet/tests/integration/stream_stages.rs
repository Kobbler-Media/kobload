use std::io::{self, Write};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::http::StatusCode;
use kob_usenet::vendor::nntp::TRACE_TARGET;
use kob_usenet::vendor::testkit::{Impairment, LoopbackServer};
use usenet_proto::Purpose;

use crate::support::{Fixture, Harness, collect, metric, output, token};

const STAGES: &str = "kobload_usenet_stream_stage_seconds";
const ROUTES: &str = "kobload_usenet_stream_connection_routes_total";
const BYTES: &str = "kobload_usenet_stream_body_bytes_total";
const REPLY_DELAY: Duration = Duration::from_millis(40);
const ADDITIVE: [&str; 8] = [
    "retry",
    "admission",
    "connection_wait",
    "send_queue",
    "pipeline_wait",
    "provider_first_byte",
    "body_transfer",
    "decode",
];

#[derive(Clone, Default)]
struct Captured(Arc<Mutex<Vec<u8>>>);

impl Write for Captured {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl Captured {
    fn trace_lines(&self) -> Vec<String> {
        String::from_utf8(self.0.lock().unwrap().clone())
            .unwrap()
            .lines()
            .filter(|line| line.contains(TRACE_TARGET))
            .map(str::to_owned)
            .collect()
    }
}

fn stage(text: &str, suffix: &str, workload: &str, stage: &str) -> f64 {
    let prefix = format!("{STAGES}_{suffix}{{workload=\"{workload}\",stage=\"{stage}\"}} ");
    text.lines()
        .find_map(|line| line.strip_prefix(prefix.as_str()))
        .unwrap_or_else(|| panic!("missing {prefix}"))
        .parse()
        .unwrap()
}

async fn delayed_read(harness: &Harness, fixture: &Fixture, purpose: Purpose) {
    let release = harness.import(fixture, "idx:guid").await;
    let index = output(&release, "Feature.2026.mkv");
    let response = harness
        .stream(
            "GET",
            &token(&release.release_key, index, purpose),
            Some("bytes=40000-99999"),
        )
        .await;
    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(collect(response).await.1, None);
}

async fn delayed_server(fixture: &Fixture) -> LoopbackServer {
    LoopbackServer::start_impaired(
        fixture.articles(),
        Impairment {
            handshake_delay: Duration::from_millis(5),
            response_delay: REPLY_DELAY,
            bandwidth: None,
        },
    )
    .await
    .unwrap()
}

#[tokio::test]
async fn a_stream_read_reports_stages_that_add_up_to_what_the_client_waited() {
    let fixture = Fixture::feature();
    let nntp = delayed_server(&fixture).await;
    let dir = tempfile::tempdir().unwrap();
    let harness = Harness::new(dir.path(), &nntp).await;
    delayed_read(&harness, &fixture, Purpose::Play).await;

    let text = harness.metrics().await;
    let delivered = stage(&text, "count", "stream", "total");
    assert!(delivered >= 1.0);
    let parts: f64 = ADDITIVE
        .iter()
        .map(|name| stage(&text, "sum", "stream", name))
        .sum();
    let total = stage(&text, "sum", "stream", "total");
    assert!((parts - total).abs() < 0.01, "{parts} vs {total}");
    assert!(
        stage(&text, "sum", "stream", "provider_first_byte") >= REPLY_DELAY.as_secs_f64() * 0.9
    );
    assert_eq!(stage(&text, "count", "stream", "service"), 1.0);
    assert!(stage(&text, "sum", "stream", "service") >= REPLY_DELAY.as_secs_f64() * 0.9);
    assert!(metric(&text, ROUTES, &["workload=\"stream\""]) >= delivered as u64);
    assert!(metric(&text, BYTES, &["workload=\"stream\""]) > 0);
    assert!(text.contains(&format!(
        "{STAGES}_bucket{{workload=\"stream\",stage=\"total\",le=\"16\"}}"
    )));
    assert!(text.contains(
        "kobload_usenet_stream_assign_load_count{workload=\"stream\",measure=\"busy_connections\"}"
    ));
    assert_eq!(stage(&text, "count", "cache", "total"), 0.0);
    assert!(!text.contains(fixture.message_id(0, 0)));
}

#[tokio::test]
async fn a_cache_read_is_labelled_apart_from_playback() {
    let fixture = Fixture::feature();
    let nntp = delayed_server(&fixture).await;
    let dir = tempfile::tempdir().unwrap();
    let harness = Harness::new(dir.path(), &nntp).await;
    delayed_read(&harness, &fixture, Purpose::Cache).await;
    let text = harness.metrics().await;
    assert!(stage(&text, "count", "cache", "total") >= 1.0);
    assert_eq!(stage(&text, "count", "cache", "service"), 1.0);
    assert_eq!(stage(&text, "count", "stream", "total"), 0.0);
}

#[tokio::test]
async fn the_trace_logs_one_clean_line_per_request_only_when_switched_on() {
    for on in [false, true] {
        let captured = Captured::default();
        let writer = captured.clone();
        let subscriber = tracing_subscriber::fmt()
            .with_max_level(tracing::Level::INFO)
            .with_ansi(false)
            .with_writer(move || writer.clone())
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        let fixture = Fixture::feature();
        let nntp = delayed_server(&fixture).await;
        let dir = tempfile::tempdir().unwrap();
        let harness = Harness::new(dir.path(), &nntp).await;
        harness.engine.stream_stages().set_trace(on);
        delayed_read(&harness, &fixture, Purpose::Play).await;

        let lines = captured.trace_lines();
        if !on {
            assert_eq!(lines, Vec::<String>::new());
            continue;
        }
        let text = harness.metrics().await;
        let delivered = stage(&text, "count", "stream", "total") as usize;
        let stages = lines.iter().filter(|l| l.contains("stream stages")).count();
        assert_eq!(stages, delivered);
        assert_eq!(
            lines
                .iter()
                .filter(|l| l.contains("stream service"))
                .count(),
            1
        );
        for line in &lines {
            for secret in [fixture.message_id(0, 0), "127.0.0.1", "hunter2", "/stream/"] {
                assert!(!line.contains(secret), "{line}");
            }
        }
    }
}
