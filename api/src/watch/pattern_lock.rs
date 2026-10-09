//! Per-pattern in-process search lock.
//!
//! Serialises concurrent search+dispatch regions for the same `watch_pattern_id`
//! so two triggers (want, backlog_search, RSS, manual, research re-search)
//! can't each pick a different release and both dispatch downloads. The
//! existing same-GUID dedup in `create_match_if_not_exists` only catches the
//! case where both sessions happen to choose identical releases — this lock
//! closes the different-release race.
//!
//! In-process only: kobload runs as a single api replica, so a tokio mutex
//! keyed by pattern_id is sufficient. If kobload ever scales horizontally this
//! needs to become a Postgres advisory lock.

use dashmap::DashMap;
use std::sync::Arc;
use tokio::sync::{Mutex, OwnedMutexGuard};
use uuid::Uuid;

/// Per-pattern lock registry.
///
/// `acquire` blocks until the pattern's lock is free; `try_acquire` returns
/// `None` immediately if the pattern is already locked — the right choice for
/// periodic jobs (backlog, RSS) that can safely skip and retry next cycle.
#[derive(Clone, Default)]
pub struct PatternSearchLocks {
    locks: Arc<DashMap<Uuid, Arc<Mutex<()>>>>,
}

impl PatternSearchLocks {
    pub fn new() -> Self {
        Self::default()
    }

    /// Acquire the lock for `pattern_id`, blocking until it's available.
    /// Use for user-triggered paths (want, manual download, research re-search)
    /// that must not drop the work.
    pub async fn acquire(&self, pattern_id: Uuid) -> OwnedMutexGuard<()> {
        let mutex = self
            .locks
            .entry(pattern_id)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone();
        mutex.lock_owned().await
    }

    /// Try to acquire the lock without blocking. Returns `None` if another
    /// session holds it. Use for periodic jobs (backlog, RSS) that can
    /// safely skip this tick and try again next cycle.
    pub fn try_acquire(&self, pattern_id: Uuid) -> Option<OwnedMutexGuard<()>> {
        let mutex = self
            .locks
            .entry(pattern_id)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone();
        mutex.try_lock_owned().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    #[tokio::test]
    async fn try_acquire_returns_none_when_held() {
        let locks = PatternSearchLocks::new();
        let id = Uuid::now_v7();
        let _held = locks.acquire(id).await;
        assert!(locks.try_acquire(id).is_none());
    }

    #[tokio::test]
    async fn try_acquire_succeeds_after_release() {
        let locks = PatternSearchLocks::new();
        let id = Uuid::now_v7();
        {
            let _held = locks.acquire(id).await;
        }
        assert!(locks.try_acquire(id).is_some());
    }

    #[tokio::test]
    async fn different_patterns_do_not_contend() {
        let locks = PatternSearchLocks::new();
        let a = Uuid::now_v7();
        let b = Uuid::now_v7();
        let _ha = locks.acquire(a).await;
        assert!(locks.try_acquire(b).is_some());
    }

    #[tokio::test]
    async fn concurrent_acquire_serialises() {
        let locks = PatternSearchLocks::new();
        let id = Uuid::now_v7();
        let counter = Arc::new(AtomicU32::new(0));
        let max_concurrent = Arc::new(AtomicU32::new(0));

        let mut handles = Vec::new();
        for _ in 0..8 {
            let locks = locks.clone();
            let counter = counter.clone();
            let max_concurrent = max_concurrent.clone();
            handles.push(tokio::spawn(async move {
                let _guard = locks.acquire(id).await;
                let now = counter.fetch_add(1, Ordering::SeqCst) + 1;
                max_concurrent.fetch_max(now, Ordering::SeqCst);
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
                counter.fetch_sub(1, Ordering::SeqCst);
            }));
        }
        for h in handles {
            h.await.unwrap();
        }
        assert_eq!(max_concurrent.load(Ordering::SeqCst), 1);
    }
}
