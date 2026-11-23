use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use kob_usenet::{Engine, ProviderStats};

use crate::throttle::Throttle;

const WARN_EVERY: Duration = Duration::from_secs(600);

pub struct Sample<'a> {
    pub key: &'a str,
    pub name: &'a str,
    pub connect_failures: u64,
    pub connect_error: Option<&'a str>,
}

impl<'a> From<&'a ProviderStats> for Sample<'a> {
    fn from(stats: &'a ProviderStats) -> Self {
        Self {
            key: &stats.key,
            name: &stats.name,
            connect_failures: stats.connect_failures,
            connect_error: stats.connect_error.as_deref(),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Failing {
    pub name: String,
    pub reason: String,
    pub failures: u64,
    pub held_back: u64,
}

// Every provider failing its login keeps /healthz green (readiness only means configured),
// so the failure has to surface in the log on its own.
#[derive(Default)]
pub struct ProviderWatch {
    seen: HashMap<String, u64>,
    throttles: HashMap<String, Throttle>,
}

impl ProviderWatch {
    pub fn observe<'a>(
        &mut self,
        samples: impl IntoIterator<Item = Sample<'a>>,
        now: Instant,
    ) -> Vec<Failing> {
        let mut failing = Vec::new();
        for sample in samples {
            let before = self
                .seen
                .insert(sample.key.to_owned(), sample.connect_failures)
                .unwrap_or(0);
            let Some(reason) = sample.connect_error else {
                continue;
            };
            if sample.connect_failures <= before {
                continue;
            }
            let throttle = self
                .throttles
                .entry(sample.key.to_owned())
                .or_insert_with(|| Throttle::new(WARN_EVERY));
            if let Some(held_back) = throttle.due(now) {
                failing.push(Failing {
                    name: sample.name.to_owned(),
                    reason: reason.to_owned(),
                    failures: sample.connect_failures - before,
                    held_back,
                });
            }
        }
        failing
    }
}

pub async fn watch_providers(engine: Arc<Engine>, every: Duration) {
    let mut watch = ProviderWatch::default();
    let mut ticks = tokio::time::interval(every);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        ticks.tick().await;
        let stats = engine.stats();
        for failing in watch.observe(stats.iter().map(Sample::from), Instant::now()) {
            tracing::warn!(
                provider = failing.name,
                reason = failing.reason,
                failures = failing.failures,
                held_back = failing.held_back,
                "usenet provider cannot connect or log in"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(failures: u64, error: Option<&str>) -> Sample<'_> {
        Sample {
            key: "a",
            name: "provider a",
            connect_failures: failures,
            connect_error: error,
        }
    }

    #[test]
    fn a_provider_failing_its_login_warns_once_per_period() {
        let start = Instant::now();
        let mut watch = ProviderWatch::default();
        let reason = "The server rejected the username or password: 481 no";
        let refused = Some(reason);
        let at = |s| start + Duration::from_secs(s);

        assert_eq!(watch.observe([sample(0, None)], at(0)), []);
        let first = watch.observe([sample(3, refused)], at(30));
        assert_eq!(
            first,
            [Failing {
                name: "provider a".to_owned(),
                reason: reason.to_owned(),
                failures: 3,
                held_back: 0,
            }]
        );
        assert_eq!(watch.observe([sample(5, refused)], at(60)), []);
        assert_eq!(watch.observe([sample(5, refused)], at(700)), []);
        let again = watch.observe([sample(6, refused)], at(701));
        assert_eq!(again.len(), 1);
        assert_eq!(again[0].held_back, 1);
    }

    #[test]
    fn a_provider_with_a_live_connection_is_not_failing() {
        let mut watch = ProviderWatch::default();
        assert_eq!(watch.observe([sample(4, None)], Instant::now()), []);
    }
}
