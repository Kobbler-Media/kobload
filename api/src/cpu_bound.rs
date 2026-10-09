/// Run a CPU-bound, await-free closure without starving the async runtime.
///
/// On the multi-threaded runtime (prod `main`), `block_in_place` lets tokio
/// move other tasks off this worker so a heavy scoring sweep can't park a
/// worker thread and stall the JetStream consumers past their idle-heartbeat
/// window. On a current-thread runtime (`#[tokio::test]` default) or outside
/// any runtime (plain `#[test]`), `block_in_place` panics, so fall back to
/// running inline — those callers are tiny batches with no consumers to starve.
pub fn run_cpu_bound<F, R>(f: F) -> R
where
    F: FnOnce() -> R,
{
    use tokio::runtime::{Handle, RuntimeFlavor};
    match Handle::try_current().map(|h| h.runtime_flavor()) {
        Ok(RuntimeFlavor::MultiThread) => tokio::task::block_in_place(f),
        _ => f(),
    }
}
