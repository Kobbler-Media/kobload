use std::sync::Mutex;
use std::time::{Duration, Instant};

pub struct Throttle {
    every: Duration,
    state: Mutex<(Option<Instant>, u64)>,
}

impl Throttle {
    pub const fn new(every: Duration) -> Self {
        Self {
            every,
            state: Mutex::new((None, 0)),
        }
    }

    pub fn due(&self, now: Instant) -> Option<u64> {
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let (last, held) = &mut *state;
        if last.is_some_and(|at| now.duration_since(at) < self.every) {
            *held += 1;
            return None;
        }
        *last = Some(now);
        Some(std::mem::take(held))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn logs_at_most_once_per_period_and_counts_what_it_held_back() {
        let start = Instant::now();
        let throttle = Throttle::new(Duration::from_secs(60));
        let due: Vec<Option<u64>> = [0, 1, 30, 59, 60, 61, 120]
            .into_iter()
            .map(|s| throttle.due(start + Duration::from_secs(s)))
            .collect();
        assert_eq!(due, [Some(0), None, None, None, Some(3), None, Some(1)]);
    }
}
