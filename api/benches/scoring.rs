//! RSS scoring benchmarks over a captured production corpus.
//!
//! Needs `KOBLOAD_SCORING_CORPUS` (see `docs/scoring-performance.md`); without
//! it the benchmarks are skipped. To approximate a small device, pin to one
//! core and cap memory:
//!
//!   KOBLOAD_SCORING_CORPUS=/path/to/capture \
//!     systemd-run --user --scope -p MemoryMax=256M -p AllowedCPUs=0 \
//!     cargo bench --bench scoring

use std::path::PathBuf;

use criterion::{Criterion, criterion_group, criterion_main};
use kobload_api::quality::parse_quality;
use kobload_api::watch::capture::{overlapping_feeds, read_capture};
use kobload_api::watch::gate_memo::MatchGateMemo;
use kobload_api::watch::rss_scoring_pass;

/// Newznab's RSS page size.
const RSS_PAGE: usize = 100;

fn scoring(c: &mut Criterion) {
    let Some(dir) = std::env::var_os("KOBLOAD_SCORING_CORPUS").map(PathBuf::from) else {
        eprintln!("KOBLOAD_SCORING_CORPUS not set; skipping scoring benchmarks");
        return;
    };
    // A pass over the whole corpus takes ~15 minutes on one core; every Nth
    // fetch keeps the mix of sources and times at a fraction of the cost.
    let step = std::env::var("KOBLOAD_SCORING_STEP")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);
    let full = read_capture(&dir).expect("read capture directory");
    // Feeds repeat releases only between consecutive polls, so the
    // overlapping-feed benches take a contiguous run of the same length
    // instead of every Nth fetch.
    let run = &full[..full.len() / step];
    let feeds = overlapping_feeds(run, RSS_PAGE);
    let corpus: Vec<_> = full.iter().step_by(step).collect();
    let titles: Vec<&str> = corpus
        .iter()
        .flat_map(|f| f.fetch.releases.iter().map(|r| r.title.as_str()))
        .collect();

    let mut group = c.benchmark_group("rss");
    // A full pass over a large corpus takes seconds; criterion's default of
    // 100 samples would take far too long.
    group.sample_size(10);
    group.bench_function("scoring_pass_whole_corpus", |b| {
        b.iter(|| {
            for f in &corpus {
                std::hint::black_box(rss_scoring_pass(
                    &f.prepared,
                    &f.fetch.releases,
                    f.fetch.source_id,
                    f.trash.as_deref(),
                    f.fetch.captured_at,
                    None,
                ));
            }
        })
    });
    for (name, reuse) in [
        ("overlapping_feeds_gates_recomputed", false),
        ("overlapping_feeds_gates_reused", true),
    ] {
        group.bench_function(name, |b| {
            b.iter(|| {
                let mut memo = MatchGateMemo::default();
                for (f, feed) in run.iter().zip(&feeds) {
                    std::hint::black_box(rss_scoring_pass(
                        &f.prepared,
                        feed,
                        f.fetch.source_id,
                        f.trash.as_deref(),
                        f.fetch.captured_at,
                        reuse.then_some(&mut memo),
                    ));
                }
            })
        });
    }
    group.bench_function("parse_quality_all_titles", |b| {
        b.iter(|| {
            for title in &titles {
                std::hint::black_box(parse_quality(title));
            }
        })
    });
    group.finish();
}

criterion_group!(benches, scoring);
criterion_main!(benches);
