//! Heap allocations made by the RSS scoring pass over a captured corpus.
//!
//! Skipped unless `KOBLOAD_SCORING_CORPUS` is set. Allocation counts don't
//! depend on machine load the way timings do, and they predict behaviour on
//! small hardware, so they are the number to watch when optimising.
//! `KOBLOAD_SCORING_ALLOC_BUDGET` (total allocations) turns the report into a
//! limit, so a regression fails instead of quietly undoing the work.
//!
//! A bench rather than part of the integration test binary: dhat replaces the
//! global allocator and counts every thread's allocations, so it needs the
//! process to itself.
//!
//!   KOBLOAD_SCORING_CORPUS=/path/to/capture cargo bench --bench scoring_alloc

use std::path::PathBuf;

use kobload_api::watch::capture::read_capture;
use kobload_api::watch::rss_scoring_pass;

#[global_allocator]
static ALLOC: dhat::Alloc = dhat::Alloc;

fn main() {
    let Some(dir) = std::env::var_os("KOBLOAD_SCORING_CORPUS").map(PathBuf::from) else {
        eprintln!("KOBLOAD_SCORING_CORPUS not set; skipping allocation profile");
        return;
    };
    // Loaded before profiling starts, so only scoring is counted.
    // A pass over the whole corpus takes ~15 minutes on one core; every Nth
    // fetch keeps the mix of sources and times at a fraction of the cost.
    let step = std::env::var("KOBLOAD_SCORING_STEP")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1);
    let corpus: Vec<_> = read_capture(&dir)
        .expect("read capture directory")
        .into_iter()
        .step_by(step)
        .collect();

    let profiler = dhat::Profiler::builder().testing().build();
    let (mut pairs, mut fully_scored) = (0, 0);
    for f in &corpus {
        let stats = rss_scoring_pass(
            &f.prepared,
            &f.fetch.releases,
            f.fetch.source_id,
            f.trash.as_deref(),
            f.fetch.captured_at,
            None,
        );
        pairs += stats.pairs;
        fully_scored += stats.fully_scored;
    }
    let heap = dhat::HeapStats::get();
    drop(profiler);

    eprintln!(
        "{} fetches, {pairs} pairs, {fully_scored} fully scored: {} allocations, {} bytes \
         allocated in total, peak heap {} bytes ({} allocations per pair)",
        corpus.len(),
        heap.total_blocks,
        heap.total_bytes,
        heap.max_bytes,
        heap.total_blocks / (pairs.max(1) as u64),
    );

    if let Some(budget) = std::env::var("KOBLOAD_SCORING_ALLOC_BUDGET")
        .ok()
        .and_then(|b| b.parse::<u64>().ok())
    {
        assert!(
            heap.total_blocks <= budget,
            "{} allocations exceeds the budget of {budget}",
            heap.total_blocks
        );
    }
}
