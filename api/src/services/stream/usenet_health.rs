use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use dashmap::DashMap;
use sqlx::PgPool;
use tracing::{info, warn};
use uuid::Uuid;

use super::outcomes::record_report;
use crate::usenet::UsenetEngine;
use nats_jobs::types::StreamOutcomeReport;

const SAMPLE_PERCENT: u8 = 5;
const RESAMPLE_AFTER: Duration = Duration::from_secs(6 * 3600);
const MISSING: &str = "missing_articles";

type SampleKey = (Uuid, String, usize);

static SAMPLED: LazyLock<DashMap<SampleKey, Instant>> = LazyLock::new(DashMap::new);

pub struct Sampled {
    pub pool: PgPool,
    pub engine: Arc<UsenetEngine>,
    pub provider_id: Uuid,
    pub release_key: String,
    pub output: usize,
    pub release_title: Option<String>,
}

/// The STAT sample runs after the resolve answers: it can take seconds on a large file, and
/// the viewer is already watching.
pub fn spawn(sampled: Sampled) {
    let key = (
        sampled.provider_id,
        sampled.release_key.clone(),
        sampled.output,
    );
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
    SAMPLED.retain(|_, at| at.elapsed() < RESAMPLE_AFTER);
    tokio::spawn(async move {
        if !sample(&sampled).await {
            SAMPLED.remove(&key);
        }
    });
}

// A stream reads straight from the articles and cannot repair, so any missing article in
// the played file demotes it. Only that file: a pack's damage is per file.
async fn sample(s: &Sampled) -> bool {
    let sample = match s
        .engine
        .sample_health(&s.release_key, SAMPLE_PERCENT, s.output)
        .await
    {
        Ok(sample) => sample,
        Err(e) => {
            warn!(release_key = %s.release_key, output = s.output, error = %e, "Usenet health sample failed");
            return false;
        }
    };
    info!(
        release_key = %s.release_key,
        output = s.output,
        checked = sample.checked,
        missing = sample.missing,
        "Usenet health sampled"
    );
    if sample.missing == 0 {
        return true;
    }
    let report = StreamOutcomeReport {
        debrid_provider_id: Some(s.provider_id),
        release_key: s.release_key.clone(),
        file_id: i32::try_from(s.output).ok(),
        ok: false,
        reason: Some(MISSING.to_string()),
        release_title: s.release_title.clone(),
        filename: None,
    };
    if let Err(e) = record_report(&s.pool, &report).await {
        warn!(release_key = %s.release_key, error = %e, "Could not demote a damaged usenet file");
        return false;
    }
    true
}
