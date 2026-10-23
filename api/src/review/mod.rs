//! The review queue: wants that got nothing past their date, and grabs that
//! may be the wrong release. Kept quiet on purpose; the user looks when the
//! nav badge says there is something to look at.

pub mod actions;
mod doubtful;
mod missed;
mod similarity;
mod want;

use serde::{Deserialize, Serialize};
use time::OffsetDateTime;
use uuid::Uuid;

use crate::repositories::release_sightings::Sighting;
use crate::services::want::scoring::MatchExplanation;
use crate::state::AppState;
use crate::watch::Release;

pub use want::want_label;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ItemKind {
    Missed,
    Doubtful,
}

impl ItemKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Missed => "missed",
            Self::Doubtful => "doubtful",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Candidate {
    #[serde(flatten)]
    pub sighting: Sighting,
    pub similarity: f64,
    pub explanation: Option<MatchExplanation>,
    pub suggested_alias: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunnerUp {
    pub watch_pattern_id: Uuid,
    pub want: String,
    pub match_score: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Grab {
    pub watch_match_id: Uuid,
    pub release_key: String,
    pub title: String,
    pub match_score: f64,
    pub matched_pattern: Option<String>,
    pub explanation: Option<MatchExplanation>,
    /// Tokens in the name part of the title the sport matcher could not place.
    pub unexplained: Vec<String>,
    pub runner_up: Option<RunnerUp>,
    pub suggested_alias: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Detail {
    pub want: String,
    pub media_kind: String,
    #[serde(default, with = "time::serde::rfc3339::option")]
    pub due_at: Option<OffsetDateTime>,
    #[serde(default)]
    pub candidates: Vec<Candidate>,
    #[serde(default)]
    pub grab: Option<Grab>,
}

impl Detail {
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::to_value(self).expect("a review detail has only string map keys")
    }
}

#[derive(Debug, Default, Serialize)]
pub struct RefreshStats {
    pub missed: usize,
    pub doubtful: usize,
    pub resolved: u64,
}

/// Bring the queue up to date. Idempotent: a want gets at most one item of
/// each kind, ever.
pub async fn refresh(
    state: &AppState,
    now: OffsetDateTime,
) -> Result<RefreshStats, crate::repositories::RepositoryError> {
    let resolved = crate::repositories::review::resolve_missed_with_grabs(&state.pool).await?;
    let missed = missed::generate(state, now).await?;
    let doubtful = doubtful::generate(state, now).await?;
    Ok(RefreshStats {
        missed,
        doubtful,
        resolved,
    })
}

fn release_of(s: &Sighting) -> Release {
    Release {
        guid: s.release_key.clone(),
        title: s.title.clone(),
        file_name: None,
        download_url: s.download_url.clone(),
        magnet_url: s.magnet_url.clone(),
        info_url: None,
        size: s.size.unwrap_or(0),
        publish_date: s
            .published_at
            .and_then(|t| {
                t.format(&time::format_description::well_known::Rfc3339)
                    .ok()
            })
            .unwrap_or_default(),
        category_ids: Vec::new(),
        indexer: s.indexer.clone(),
        protocol: s.protocol.clone(),
        info_hash: s.info_hash.clone(),
        files: None,
        seeders: None,
        peers: None,
        leechers: None,
        grabs: None,
        votes: None,
        indexer_id: None,
        indexer_source_id: s.indexer_source_id,
        raw_data: serde_json::json!({}),
        external_ids: Vec::new(),
        media: None,
    }
}
