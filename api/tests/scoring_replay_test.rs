//! Golden comparison of RSS scoring against a captured production corpus.
//!
//! Skipped unless `KOBLOAD_SCORING_CORPUS` points at a capture directory (see
//! `docs/scoring-performance.md`). The first run, or any run with
//! `KOBLOAD_SCORING_BLESS=1`, writes `golden.jsonl.gz` into that directory;
//! later runs must reproduce it exactly. Bless only after reviewing every
//! reported difference.
//!
//!   KOBLOAD_SCORING_CORPUS=/path/to/capture \
//!     cargo test --release --test scoring_replay_test -- --nocapture

use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;

use flate2::Compression;
use flate2::read::MultiGzDecoder;
use flate2::write::GzEncoder;
use kobload_api::watch::capture::read_capture;
use kobload_api::watch::replay_scoring;
use serde_json::Value;

const FLOAT_TOLERANCE: f64 = 1e-9;
const MAX_REPORTED_DIFFS: usize = 25;

fn corpus_dir() -> Option<PathBuf> {
    std::env::var_os("KOBLOAD_SCORING_CORPUS").map(PathBuf::from)
}

#[test]
fn rss_scoring_matches_golden_snapshot() {
    let Some(dir) = corpus_dir() else {
        eprintln!("KOBLOAD_SCORING_CORPUS not set; skipping golden scoring replay");
        return;
    };
    let corpus = read_capture(&dir).expect("read capture directory");
    assert!(!corpus.is_empty(), "capture has no fetches");

    let started = Instant::now();
    let (mut releases, mut pairs, mut fully_scored) = (0, 0, 0);
    let replays: Vec<Value> = corpus
        .iter()
        .map(|f| {
            let replay = replay_scoring(
                &f.prepared,
                &f.fetch.releases,
                f.fetch.source_id,
                f.trash.as_deref(),
                f.fetch.captured_at,
            );
            releases += f.fetch.releases.len();
            pairs += replay.stats.pairs;
            fully_scored += replay.stats.fully_scored;
            let mut value = serde_json::to_value(&replay).unwrap();
            value["fetch"] = format!(
                "{} {} {}",
                f.fetch.captured_at, f.fetch.source_name, f.fetch.source_id
            )
            .into();
            value
        })
        .collect();
    eprintln!(
        "replayed {} fetches: {releases} releases, {pairs} pairs, {fully_scored} fully scored, \
         {:.1?} (includes the unoptimised reference pass)",
        corpus.len(),
        started.elapsed()
    );

    let golden_path = dir.join("golden.jsonl.gz");
    let bless = std::env::var("KOBLOAD_SCORING_BLESS").is_ok_and(|v| v == "1");
    if bless || !golden_path.exists() {
        write_golden(&golden_path, &replays);
        eprintln!("wrote {}", golden_path.display());
        return;
    }

    let golden = read_golden(&golden_path);
    assert_eq!(
        golden.len(),
        replays.len(),
        "golden has {} fetches, replay has {}; the corpus changed, so re-bless",
        golden.len(),
        replays.len()
    );

    let mut diffs = Vec::new();
    for (i, (want, got)) in golden.iter().zip(&replays).enumerate() {
        compare(want, got, &format!("fetch[{i}] ({})", got["fetch"]), &mut diffs);
    }
    assert!(
        diffs.is_empty(),
        "{} difference(s) from the golden snapshot (first {} shown):\n{}",
        diffs.len(),
        diffs.len().min(MAX_REPORTED_DIFFS),
        diffs
            .iter()
            .take(MAX_REPORTED_DIFFS)
            .cloned()
            .collect::<Vec<_>>()
            .join("\n")
    );
}

fn compare(want: &Value, got: &Value, path: &str, diffs: &mut Vec<String>) {
    match (want, got) {
        (Value::Number(a), Value::Number(b)) => {
            let (a, b) = (a.as_f64().unwrap_or(f64::NAN), b.as_f64().unwrap_or(f64::NAN));
            if (a - b).abs() > FLOAT_TOLERANCE {
                diffs.push(format!("{path}: {a} -> {b}"));
            }
        }
        (Value::Array(a), Value::Array(b)) => {
            if a.len() != b.len() {
                diffs.push(format!(
                    "{path}: length {} -> {}\n  golden: {}\n  replay: {}",
                    a.len(),
                    b.len(),
                    summarise(want),
                    summarise(got)
                ));
                return;
            }
            for (i, (x, y)) in a.iter().zip(b).enumerate() {
                compare(x, y, &format!("{path}[{i}]{}", label(x)), diffs);
            }
        }
        (Value::Object(a), Value::Object(b)) => {
            for key in a.keys().chain(b.keys().filter(|k| !a.contains_key(*k))) {
                let (x, y) = (
                    a.get(key).unwrap_or(&Value::Null),
                    b.get(key).unwrap_or(&Value::Null),
                );
                compare(x, y, &format!("{path}.{key}"), diffs);
            }
        }
        _ if want != got => diffs.push(format!("{path}: {want} -> {got}")),
        _ => {}
    }
}

/// Identify a matched pair or candidate in a diff path.
fn label(v: &Value) -> String {
    match (v.get("pattern_id"), v.get("release").or_else(|| v.get("guid"))) {
        (Some(p), Some(r)) => format!("{{pattern {p}, release {r}}}"),
        _ => String::new(),
    }
}

fn summarise(v: &Value) -> String {
    let s = v.to_string();
    if s.len() > 400 {
        format!("{}…", &s[..s.floor_char_boundary(400)])
    } else {
        s
    }
}

fn write_golden(path: &Path, replays: &[Value]) {
    let mut out = GzEncoder::new(File::create(path).unwrap(), Compression::default());
    for replay in replays {
        serde_json::to_writer(&mut out, replay).unwrap();
        out.write_all(b"\n").unwrap();
    }
    out.finish().unwrap();
}

fn read_golden(path: &Path) -> Vec<Value> {
    BufReader::new(MultiGzDecoder::new(File::open(path).unwrap()))
        .lines()
        .map(|l| serde_json::from_str(&l.unwrap()).unwrap())
        .collect()
}
