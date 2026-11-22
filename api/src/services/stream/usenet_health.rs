use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use dashmap::DashMap;
use sqlx::PgPool;
use tracing::{info, warn};
use uuid::Uuid;

use super::outcomes::record_report;
use super::warm_cache::record_availability;
use crate::repositories::usenet_releases;
use crate::usenet::UsenetEngine;
use nats_jobs::types::StreamOutcomeReport;
use usenet_proto::HealthSample;

const SAMPLE_PERCENT: u8 = 5;
/// PAR2 sets are usually posted at 5–10% recovery, so a sample missing more than this is
/// past what they would repair.
const REPAIRABLE_PERCENT: usize = 2;
const RESAMPLE_AFTER: Duration = Duration::from_secs(6 * 3600);
const MISSING: &str = "missing_articles";

static SAMPLED: LazyLock<DashMap<(Uuid, String), Instant>> = LazyLock::new(DashMap::new);

pub struct Sampled {
    pub pool: PgPool,
    pub engine: Arc<UsenetEngine>,
    pub provider_id: Uuid,
    pub release_key: String,
    pub file_id: Option<i32>,
    pub release_title: Option<String>,
}

pub fn damaged(sample: HealthSample, has_par2: bool) -> bool {
    sample.missing > 0 && (!has_par2 || sample.missing * 100 > sample.checked * REPAIRABLE_PERCENT)
}

/// The STAT sample runs after the resolve answers: it can take seconds on a pack, and the
/// viewer is already watching.
pub fn spawn(sampled: Sampled) {
    let key = (sampled.provider_id, sampled.release_key.clone());
    let fresh = match SAMPLED.entry(key.clone()) {
        dashmap::Entry::Occupied(mut e) if e.get().elapsed() >= RESAMPLE_AFTER => {
            e.insert(Instant::now());
            true
        }
        dashmap::Entry::Occupied(_) => false,
        dashmap::Entry::Vacant(v) => {
            v.insert(Instant::now());
            true
        }
    };
    if !fresh {
        return;
    }
    tokio::spawn(async move {
        if !sample(&sampled).await {
            SAMPLED.remove(&key);
        }
    });
}

async fn sample(s: &Sampled) -> bool {
    let sample = match s.engine.sample_health(&s.release_key, SAMPLE_PERCENT).await {
        Ok(sample) => sample,
        Err(e) => {
            warn!(release_key = %s.release_key, error = %e, "Usenet health sample failed");
            return false;
        }
    };
    let has_par2 = match usenet_releases::has_par2(&s.pool, &s.release_key).await {
        Ok(has_par2) => has_par2.unwrap_or(false),
        Err(e) => {
            warn!(release_key = %s.release_key, error = %e, "Could not read whether a release has PAR2");
            return false;
        }
    };
    let damaged = damaged(sample, has_par2);
    info!(
        release_key = %s.release_key,
        checked = sample.checked,
        missing = sample.missing,
        has_par2,
        damaged,
        "Usenet health sampled"
    );
    if !damaged {
        record_availability(&s.pool, &s.release_key, s.provider_id, None).await;
        return true;
    }
    let report = StreamOutcomeReport {
        debrid_provider_id: Some(s.provider_id),
        release_key: s.release_key.clone(),
        file_id: s.file_id,
        ok: false,
        reason: Some(MISSING.to_string()),
        release_title: s.release_title.clone(),
        filename: None,
    };
    if let Err(e) = record_report(&s.pool, &report).await {
        warn!(release_key = %s.release_key, error = %e, "Could not demote a damaged usenet release");
        return false;
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_missing_articles_past_repair_count_as_damage() {
        let cases = [
            (0, 40, false, false),
            (1, 40, false, true),
            (2, 100, true, false),
            (3, 100, true, true),
            (1, 1, true, true),
        ];
        for (missing, checked, has_par2, expected) in cases {
            assert_eq!(
                damaged(HealthSample { checked, missing }, has_par2),
                expected,
                "{missing}/{checked} par2={has_par2}"
            );
        }
    }
}
