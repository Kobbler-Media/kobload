use std::collections::HashMap;

use sqlx::PgPool;
use time::{Duration, OffsetDateTime};
use tracing::warn;
use uuid::Uuid;

use super::binge;
use super::warm_cache::{UnavailableReason, record_availability};
use crate::repositories::RepositoryError;
use crate::repositories::stream_outcomes::{self as repo, TraitDecay};
use nats_jobs::types::{StreamOutcomeReport, StreamPlayback};

pub const FAILURE_MEMORY_HOURS: i32 = 72;
pub const SUCCESS_MEMORY_DAYS: i32 = 30;
const HALF_LIFE_SECS: f64 = 7.0 * 86_400.0;
const DECAY: TraitDecay = TraitDecay {
    half_life_secs: HALF_LIFE_SECS,
    cap: 20.0,
};
const PRUNE_BELOW_WEIGHT: f64 = 0.05;

/// Pseudo-counts (failures, successes) a trait starts from. The default sits at a 1-in-7
/// failure rate so a single bad report cannot move a release group on its own.
const DEFAULT_PRIOR: (f64, f64) = (0.5, 3.0);
/// Real-Debrid's filter-gate (error_code 35) refuses releases by filename, and these tags
/// are what it was seen targeting. Starting them at 2-in-3 keeps cold-start ranking where
/// the old static penalty had it, while letting real successes argue them back down.
const RD_TAG_PRIOR: (f64, f64) = (3.0, 1.5);

const MEDIUM_RISK: f64 = 0.4;
const HIGH_RISK: f64 = 0.6;

/// Canonical tag, then the spellings that mean it. Matched on whole tokens (and on
/// adjacent token pairs joined by '-') so "NF" never matches inside "NFL".
const RD_FILTER_TAGS: &[(&str, &[&str])] = &[
    ("web-dl", &["web-dl", "webdl"]),
    ("webrip", &["webrip", "web-rip"]),
    ("amzn", &["amzn"]),
    ("nf", &["nf"]),
    ("cr", &["cr"]),
    ("yts", &["yts"]),
    ("rarbg", &["rarbg"]),
    ("dsnp", &["dsnp"]),
    ("hmax", &["hmax"]),
    ("atvp", &["atvp"]),
    ("pcok", &["pcok"]),
    ("stan", &["stan"]),
];

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

#[derive(Debug, Clone, Copy)]
enum Evidence {
    Played,
    PlaybackFailed,
    ProbeAvailable,
    ProbeBlocked,
}

impl Evidence {
    /// (success, failure). A client's playback failure is weighted down because it may
    /// be that device's decoder rather than the release; a provider refusal is not.
    fn weights(self) -> (f64, f64) {
        match self {
            Self::Played => (1.0, 0.0),
            Self::PlaybackFailed => (0.0, 0.25),
            Self::ProbeAvailable => (0.5, 0.0),
            Self::ProbeBlocked => (0.0, 1.0),
        }
    }
}

pub fn release_traits(title: &str) -> Vec<String> {
    let lower = title.to_lowercase();
    let tokens: Vec<&str> = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .collect();
    let pairs = tokens.windows(2).map(|w| format!("{}-{}", w[0], w[1]));
    let candidates: Vec<String> = tokens.iter().map(|t| t.to_string()).chain(pairs).collect();

    let mut traits: Vec<String> = RD_FILTER_TAGS
        .iter()
        .filter(|(_, spellings)| candidates.iter().any(|c| spellings.contains(&c.as_str())))
        .map(|(tag, _)| format!("tag:{tag}"))
        .collect();
    if let Some(group) = crate::quality::parse_quality(title).release_group {
        traits.push(format!("group:{}", group.to_lowercase()));
    }
    traits
}

fn prior(trait_name: &str, provider_type: &str) -> (f64, f64) {
    if provider_type == "real_debrid" && trait_name.starts_with("tag:") {
        RD_TAG_PRIOR
    } else {
        DEFAULT_PRIOR
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

pub struct TraitRisk {
    rates: HashMap<String, f64>,
    provider_type: String,
}

impl TraitRisk {
    pub async fn load(
        pool: &PgPool,
        provider_id: Uuid,
        provider_type: &str,
        traits: &[String],
    ) -> Self {
        let rows = repo::traits_for(pool, provider_id, traits, HALF_LIFE_SECS)
            .await
            .unwrap_or_else(|e| {
                warn!(error = %e, "trait outcome lookup failed; ranking on priors");
                Vec::new()
            });
        let rates = rows
            .into_iter()
            .map(|r| {
                let rate = smoothed_rate(
                    prior(&r.trait_name, provider_type),
                    r.failure_weight,
                    r.success_weight,
                );
                (r.trait_name, rate)
            })
            .collect();
        Self {
            rates,
            provider_type: provider_type.to_string(),
        }
    }

    /// 0 = low, 1 = medium, 2 = high. Buckets rather than the raw rate, so learned risk
    /// separates releases without reshuffling the quality order of near-equal ones.
    pub fn bucket(&self, traits: &[String]) -> u8 {
        let risk = traits
            .iter()
            .map(|t| {
                self.rates
                    .get(t)
                    .copied()
                    .unwrap_or_else(|| smoothed_rate(prior(t, &self.provider_type), 0.0, 0.0))
            })
            .fold(smoothed_rate(DEFAULT_PRIOR, 0.0, 0.0), f64::max);
        if risk >= HIGH_RISK {
            2
        } else if risk >= MEDIUM_RISK {
            1
        } else {
            0
        }
    }
}

fn smoothed_rate((prior_fail, prior_ok): (f64, f64), failures: f64, successes: f64) -> f64 {
    (failures + prior_fail) / (failures + successes + prior_fail + prior_ok)
}

async fn add_evidence(pool: &PgPool, provider_id: Uuid, title: &str, evidence: Evidence) {
    let traits = release_traits(title);
    if traits.is_empty() {
        return;
    }
    let (success, failure) = evidence.weights();
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
            add_evidence(pool, provider_id, title, Evidence::Played).await;
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
    if let Some(title) = title {
        add_evidence(pool, provider_id, title, Evidence::PlaybackFailed).await;
    }
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
        None => Evidence::ProbeAvailable,
        Some(UnavailableReason::Blocked) => Evidence::ProbeBlocked,
        Some(UnavailableReason::Dead) => return,
    };
    if fresh && let Some(title) = release_title {
        add_evidence(pool, provider_id, title, evidence).await;
    }
}

pub async fn prune(pool: &PgPool) -> Result<(u64, u64), RepositoryError> {
    let releases = repo::prune_releases(pool, FAILURE_MEMORY_HOURS, SUCCESS_MEMORY_DAYS).await?;
    let traits = repo::prune_traits(pool, HALF_LIFE_SECS, PRUNE_BELOW_WEIGHT).await?;
    Ok((releases, traits))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_filter_tags_and_group() {
        let cases: &[(&str, &[&str])] = &[
            (
                "Movie.2024.1080p.WEB-DL.x264-SPARKS",
                &["tag:web-dl", "group:sparks"],
            ),
            (
                "Movie 2024 AMZN WEBRip-GRP",
                &["tag:webrip", "tag:amzn", "group:grp"],
            ),
            ("Movie.2024.Web.Rip.YTS.MX", &["tag:webrip", "tag:yts"]),
            ("NFL.2024.Week.1.720p", &[]),
            ("Movie.2024.BluRay.x264", &[]),
        ];
        for (title, want) in cases {
            assert_eq!(release_traits(title), *want, "{title}");
        }
    }

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
