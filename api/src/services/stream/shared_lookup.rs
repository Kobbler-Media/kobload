//! Lookups that overlapping stream searches share. One play asks for a title several times
//! within seconds (a details page, the server's prewarm, kobbler web and kobapp alike), and
//! each of those used to query the same outside services again.

use std::future::Future;
use std::sync::Arc;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use tokio::sync::OnceCell;

const SWEEP_ABOVE: usize = 200;

pub struct SharedLookups<T> {
    ttl: Duration,
    entries: DashMap<String, Arc<Entry<T>>>,
}

struct Entry<T> {
    at: Instant,
    value: OnceCell<T>,
}

impl<T: Clone> SharedLookups<T> {
    pub fn new(ttl: Duration) -> Self {
        Self {
            ttl,
            entries: DashMap::new(),
        }
    }

    /// Callers that ask for [key] while it is being fetched wait for that fetch. A value
    /// [keep] rejects (an empty or incomplete answer) still serves them, but the next caller
    /// fetches again.
    pub async fn get<F, Fut>(&self, key: &str, keep: impl Fn(&T) -> bool, fetch: F) -> T
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = T>,
    {
        let entry = self.entry(key);
        let value = entry.value.get_or_init(fetch).await.clone();
        if !keep(&value) {
            self.entries.remove_if(key, |_, e| Arc::ptr_eq(e, &entry));
        }
        value
    }

    fn entry(&self, key: &str) -> Arc<Entry<T>> {
        if self.entries.len() > SWEEP_ABOVE {
            self.entries.retain(|_, e| e.at.elapsed() < self.ttl);
        }
        let mut entry = self
            .entries
            .entry(key.to_string())
            .or_insert_with(|| Arc::new(Entry::new()));
        if entry.at.elapsed() >= self.ttl {
            *entry = Arc::new(Entry::new());
        }
        entry.clone()
    }
}

impl<T> Entry<T> {
    fn new() -> Self {
        Self {
            at: Instant::now(),
            value: OnceCell::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    async fn fetch(calls: &AtomicUsize, value: Vec<u8>) -> Vec<u8> {
        calls.fetch_add(1, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(20)).await;
        value
    }

    #[tokio::test]
    async fn overlapping_and_later_callers_share_one_fetch() {
        let shared = SharedLookups::new(Duration::from_secs(60));
        let calls = AtomicUsize::new(0);
        let keep = |v: &Vec<u8>| !v.is_empty();
        let (a, b) = tokio::join!(
            shared.get("k", keep, || fetch(&calls, vec![1])),
            shared.get("k", keep, || fetch(&calls, vec![2])),
        );
        let later = shared.get("k", keep, || fetch(&calls, vec![3])).await;
        assert_eq!((a, b, later), (vec![1], vec![1], vec![1]));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn an_answer_not_kept_or_expired_is_fetched_again() {
        let calls = AtomicUsize::new(0);
        let keep = |v: &Vec<u8>| !v.is_empty();
        let shared = SharedLookups::new(Duration::from_secs(60));
        shared.get("k", keep, || fetch(&calls, vec![])).await;
        let again = shared.get("k", keep, || fetch(&calls, vec![1])).await;
        assert_eq!(again, vec![1]);
        assert_eq!(calls.load(Ordering::SeqCst), 2);

        let short = SharedLookups::new(Duration::from_millis(30));
        short.get("k", keep, || fetch(&calls, vec![1])).await;
        tokio::time::sleep(Duration::from_millis(40)).await;
        let fresh = short.get("k", keep, || fetch(&calls, vec![2])).await;
        assert_eq!(fresh, vec![2]);
    }

    #[tokio::test]
    async fn keys_are_fetched_apart() {
        let shared = SharedLookups::new(Duration::from_secs(60));
        let calls = AtomicUsize::new(0);
        let keep = |_: &Vec<u8>| true;
        shared.get("a", keep, || fetch(&calls, vec![1])).await;
        let b = shared.get("b", keep, || fetch(&calls, vec![2])).await;
        assert_eq!(b, vec![2]);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }
}
