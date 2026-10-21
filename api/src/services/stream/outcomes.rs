use sqlx::PgPool;
use time::{Duration, OffsetDateTime};
use tracing::warn;
use uuid::Uuid;

use super::binge;
use super::trait_risk::{BASELINE, HALF_LIFE_SECS, release_traits};
use super::warm_cache::{UnavailableReason, record_availability};
use crate::repositories::stream_outcomes::{self as repo, TraitDecay};
use crate::repositories::{RepositoryError, app_settings};
use nats_jobs::types::{StreamOutcomeReport, StreamPlayback};

pub const FAILURE_MEMORY_HOURS: i32 = 72;
pub const SUCCESS_MEMORY_DAYS: i32 = 30;
pub use super::trait_risk::TRAIT_TTL_DAYS;
const DECAY: TraitDecay = TraitDecay {
    half_life_secs: HALF_LIFE_SECS,
    cap: 50.0,
};
const PRUNE_BELOW_WEIGHT: f64 = 0.05;
const INGESTED_KEY: &str = "stream_trait_ingested_providers";

const TRANSIENT_REASONS: &[&str] = &[
    "rate_limited",
    "rate limit",
    "auth_failed",
    "authentication",
    "unauthorized",
    "timeout",
    "timed out",
    "unavailable",
    "fair_usage",
    "fair usage",
    "too_many_active",
    "too many active",
    "no free slot",
];

/// Refusal risk is about whether the provider accepts a release, so only acceptance and
/// refusal count. A playback failure is usually the device, and a dead torrent says
/// nothing about its name.
#[derive(Debug, Clone, Copy)]
enum Evidence {
    Accepted,
    Refused,
}

impl Evidence {
    /// (success, failure)
    fn weights(self) -> (f64, f64) {
        match self {
            Self::Accepted => (1.0, 0.0),
            Self::Refused => (0.0, 1.0),
        }
    }
}

pub fn is_transient(reason: &str) -> bool {
    let lower = reason.to_lowercase();
    TRANSIENT_REASONS.iter().any(|t| lower.contains(t))
}

pub fn verdict(
    last_success: Option<OffsetDateTime>,
    last_failure: Option<OffsetDateTime>,
    now: OffsetDateTime,
) -> Option<StreamPlayback> {
    let success = last_success.filter(|s| now - *s < Duration::days(SUCCESS_MEMORY_DAYS.into()));
    let failure = last_failure.filter(|f| now - *f < Duration::hours(FAILURE_MEMORY_HOURS.into()));
    match (success, failure) {
        (Some(s), Some(f)) if f > s => Some(StreamPlayback::Failed),
        (Some(_), _) => Some(StreamPlayback::Worked),
        (None, Some(_)) => Some(StreamPlayback::Failed),
        (None, None) => None,
    }
}

async fn add_evidence(
    pool: &PgPool,
    provider_id: Uuid,
    title: &str,
    evidence: Evidence,
    weight: f64,
) {
    let mut traits = release_traits(title);
    traits.push(BASELINE.to_string());
    let (success, failure) = evidence.weights();
    let (success, failure) = (success * weight, failure * weight);
    if let Err(e) =
        repo::add_trait_evidence(pool, provider_id, &traits, success, failure, &DECAY).await
    {
        warn!(error = %e, "failed to record trait evidence");
    }
}

pub async fn record_report(
    pool: &PgPool,
    report: &StreamOutcomeReport,
) -> Result<(), RepositoryError> {
    let provider_id = match report.debrid_provider_id {
        Some(id) => id,
        None => match repo::default_provider(pool).await? {
            Some(id) => id,
            None => return Ok(()),
        },
    };
    let info_hash = report.info_hash.trim().to_lowercase();
    let title = report
        .release_title
        .as_deref()
        .or(report.filename.as_deref())
        .filter(|t| !t.trim().is_empty());

    if report.ok {
        let binge_group = binge::lookup(&info_hash);
        repo::record_success(
            pool,
            provider_id,
            &info_hash,
            report.file_id,
            binge_group.as_deref(),
        )
        .await?;
        if let Some(title) = title {
            add_evidence(pool, provider_id, title, Evidence::Accepted, 1.0).await;
        }
        return Ok(());
    }

    let reason = report
        .reason
        .as_deref()
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .unwrap_or("client_reported");
    if is_transient(reason) {
        return Ok(());
    }
    let reason: String = reason.chars().take(200).collect();
    repo::record_failure(pool, provider_id, &info_hash, report.file_id, &reason).await?;
    Ok(())
}

/// Persists the availability verdict and, when it is news, learns from the release name.
/// A dead torrent says nothing about its name, so only acceptance and refusal count.
pub async fn record_probe(
    pool: &PgPool,
    provider_id: Uuid,
    info_hash: &str,
    release_title: Option<&str>,
    unavailable: Option<UnavailableReason>,
) {
    let fresh = record_availability(pool, info_hash, provider_id, unavailable).await;
    let evidence = match unavailable {
        None => Evidence::Accepted,
        Some(UnavailableReason::Blocked) => Evidence::Refused,
        Some(UnavailableReason::Dead) => return,
    };
    if fresh && let Some(title) = release_title {
        add_evidence(pool, provider_id, title, evidence, 1.0).await;
    }
}

pub async fn prune(pool: &PgPool) -> Result<(u64, u64), RepositoryError> {
    let releases = repo::prune_releases(pool, FAILURE_MEMORY_HOURS, SUCCESS_MEMORY_DAYS).await?;
    let traits =
        repo::prune_traits(pool, HALF_LIFE_SECS, PRUNE_BELOW_WEIGHT, TRAIT_TTL_DAYS).await?;
    Ok((releases, traits))
}

/// Learns from availability probes recorded before trait learning existed (or before a
/// provider was added), once per provider. Each verdict is weighted down by its age as if it
/// had been learned live, and anything past the TTL is skipped.
pub async fn ingest_past_probes(pool: &PgPool) -> Result<usize, RepositoryError> {
    let done: Vec<Uuid> = app_settings::get(pool, INGESTED_KEY)
        .await?
        .and_then(|v| serde_json::from_value(v).ok())
        .unwrap_or_default();
    let mut ingested = 0;
    let mut now_done = done.clone();
    for provider_id in repo::provider_ids(pool).await? {
        if done.contains(&provider_id) {
            continue;
        }
        for probe in repo::past_probes(pool, provider_id, TRAIT_TTL_DAYS).await? {
            let evidence = if probe.available {
                Evidence::Accepted
            } else {
                Evidence::Refused
            };
            let weight = 0.5_f64.powf(probe.age_secs / HALF_LIFE_SECS);
            add_evidence(pool, provider_id, &probe.title, evidence, weight).await;
            ingested += 1;
        }
        now_done.push(provider_id);
    }
    if now_done.len() != done.len() {
        app_settings::set(pool, INGESTED_KEY, &serde_json::json!(now_done)).await?;
    }
    Ok(ingested)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn verdict_follows_the_latest_unexpired_outcome() {
        let now = OffsetDateTime::now_utc();
        let ago = |h: i64| Some(now - Duration::hours(h));
        let cases = [
            (None, None, None),
            (ago(1), None, Some(StreamPlayback::Worked)),
            (ago(1), ago(2), Some(StreamPlayback::Worked)),
            (ago(2), ago(1), Some(StreamPlayback::Failed)),
            (None, ago(1), Some(StreamPlayback::Failed)),
            (None, ago(73), None),
            (ago(100), ago(73), Some(StreamPlayback::Worked)),
            (ago(24 * 31), None, None),
        ];
        for (s, f, want) in cases {
            assert_eq!(verdict(s, f, now), want, "success {s:?} failure {f:?}");
        }
    }

    #[test]
    fn transient_reasons_are_recognised() {
        for r in [
            "rate_limited",
            "Rate limited (retry after 5s)",
            "timeout",
            "fair_usage",
        ] {
            assert!(is_transient(r), "{r}");
        }
        for r in [
            "decoder_error",
            "not_video",
            "upstream_failed",
            "probe_failed",
        ] {
            assert!(!is_transient(r), "{r}");
        }
    }
}
