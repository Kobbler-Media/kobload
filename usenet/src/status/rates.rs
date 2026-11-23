use std::collections::{HashMap, VecDeque};
use std::hash::Hash;
use std::time::{Duration, Instant};

// Several open pages must not grow the window, and two samples moments apart would read a
// download between articles as stalled.
const MIN_GAP: Duration = Duration::from_millis(250);
const MIN_SPAN: Duration = Duration::from_secs(1);

pub struct RateWindow<K> {
    span: Duration,
    samples: VecDeque<(Instant, HashMap<K, u64>)>,
}

impl<K: Eq + Hash> RateWindow<K> {
    #[must_use]
    pub const fn new(span: Duration) -> Self {
        Self {
            span,
            samples: VecDeque::new(),
        }
    }

    pub fn record(&mut self, now: Instant, counters: impl IntoIterator<Item = (K, u64)>) {
        if self
            .samples
            .back()
            .is_some_and(|(at, _)| now.duration_since(*at) < MIN_GAP)
        {
            return;
        }
        self.samples
            .retain(|(at, _)| now.duration_since(*at) <= self.span);
        self.samples
            .push_back((now, counters.into_iter().collect()));
    }

    #[must_use]
    pub fn rate(&self, key: &K) -> Option<u64> {
        let (last_at, last) = self.samples.back()?;
        let latest = *last.get(key)?;
        let (first_at, first) = self
            .samples
            .iter()
            .find_map(|(at, counters)| counters.get(key).map(|value| (*at, *value)))?;
        let span = last_at.duration_since(first_at);
        if span < MIN_SPAN {
            return None;
        }
        let per_second = u128::from(latest.saturating_sub(first)) * 1000 / span.as_millis();
        Some(u64::try_from(per_second).unwrap_or(u64::MAX))
    }
}
