use dashmap::DashMap;
use std::net::IpAddr;
use std::time::{Duration, Instant};

/// Simple in-memory IP-based rate limiter using a sliding window.
pub struct RateLimiter {
    map: DashMap<IpAddr, (u32, Instant)>,
    max_attempts: u32,
    window: Duration,
}

impl RateLimiter {
    pub fn new(max_attempts: u32, window: Duration) -> Self {
        Self {
            map: DashMap::new(),
            max_attempts,
            window,
        }
    }

    /// Check and increment the counter for the given IP.
    /// Returns `true` if under the limit, `false` if rate limited.
    pub fn check(&self, ip: IpAddr) -> bool {
        let now = Instant::now();
        let mut entry = self.map.entry(ip).or_insert((0, now));
        let (count, window_start) = entry.value_mut();

        if now.duration_since(*window_start) > self.window {
            *count = 1;
            *window_start = now;
            true
        } else if *count < self.max_attempts {
            *count += 1;
            true
        } else {
            false
        }
    }
}
