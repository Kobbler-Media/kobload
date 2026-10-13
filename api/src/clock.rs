//! Wall clock for release scoring.
//!
//! Scoring is time-dependent (freshness bonus, air-date gating, pack recency),
//! so replaying captured RSS data would drift from day to day. A replay pins
//! the clock to the capture time on its own thread; everything else gets the
//! real time.

use std::cell::Cell;

use time::OffsetDateTime;

thread_local! {
    static PINNED: Cell<Option<OffsetDateTime>> = const { Cell::new(None) };
}

pub fn now_utc() -> OffsetDateTime {
    PINNED.with(Cell::get).unwrap_or_else(OffsetDateTime::now_utc)
}

/// Run `f` with [`now_utc`] returning `now` on this thread.
pub fn with_pinned_now<R>(now: OffsetDateTime, f: impl FnOnce() -> R) -> R {
    struct Restore(Option<OffsetDateTime>);
    impl Drop for Restore {
        fn drop(&mut self) {
            PINNED.with(|p| p.set(self.0));
        }
    }
    let _restore = Restore(PINNED.with(|p| p.replace(Some(now))));
    f()
}
