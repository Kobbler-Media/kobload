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
use kobload_api::watch::capture::read_capture;
use kobload_api::watch::rss_scoring_pass;

fn scoring(c: &mut Criterion) {
    let Some(dir) = std::env::var_os("KOBLOAD_SCORING_CORPUS").map(PathBuf::from) else {
        eprintln!("KOBLOAD_SCORING_CORPUS not set; skipping scoring benchmarks");
        return;
    };
    let corpus = read_capture(&dir).expect("read capture directory");
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
                ));
            }
        })
    });
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
