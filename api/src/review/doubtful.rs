//! Grabs that may be the wrong release: another want would have taken the
//! same release nearly as readily, or the sport matcher could not place a
//! word in the name part of the title.

use nats_jobs::MediaWant;
use time::{Duration, OffsetDateTime};
use tracing::debug;

use super::similarity::best_similarity;
use super::want::{names, slot_unexplained, suggested_alias, want_label};
use super::{Detail, Grab, ItemKind, RunnerUp, release_of};
use crate::repositories::release_sightings::Sighting;
use crate::repositories::watch::{self as watch_repo, WatchPatternRow};
use crate::repositories::{RepositoryError, app_settings, review as repo};
use crate::services::want::scoring::sport_explanation;
use crate::state::AppState;
use crate::watch::{pattern_media_want, score_for_pattern};

const CHECKED_UNTIL: &str = "review_doubtful_checked_until";
/// The first run looks back this far; later runs start where the last ended.
const LOOKBACK: Duration = Duration::days(7);
/// A runner-up want this close in match score could as well have been the one.
const DOUBT_MARGIN: f64 = 0.1;
/// Only wants whose names look like the release are scored as runners-up.
const RELATED: f64 = 0.5;

struct Rival {
    pattern: WatchPatternRow,
    kind: &'static str,
    label: String,
    names: Vec<String>,
}

pub(super) async fn generate(
    state: &AppState,
    now: OffsetDateTime,
) -> Result<usize, RepositoryError> {
    let pool = &state.pool;
    let since = app_settings::get(pool, CHECKED_UNTIL)
        .await?
        .and_then(|v| {
            v.as_str()
                .and_then(crate::services::upgrade::parse_publish_date)
        })
        .unwrap_or(now - LOOKBACK)
        .max(now - LOOKBACK);
    let grabs = repo::grabs_since(pool, since).await?;
    let existing = repo::items_by_pattern(pool).await?;
    let rivals: Vec<Rival> = if grabs.is_empty() {
        Vec::new()
    } else {
        watch_repo::get_all_enabled_patterns(pool)
            .await?
            .into_iter()
            .filter_map(|pattern| {
                let want: MediaWant = serde_json::from_value(pattern.media_want.clone()?).ok()?;
                Some(Rival {
                    kind: crate::media::parse_media_kind(&want),
                    label: want_label(&want),
                    names: names(&want),
                    pattern,
                })
            })
            .collect()
    };

    let mut created = 0;
    for grab in grabs {
        if grab.matched_pattern.as_deref() == Some(repo::REVIEW_GRAB)
            || existing.contains(&(grab.watch_pattern_id, ItemKind::Doubtful.as_str().into()))
        {
            continue;
        }
        let Some(pattern) = watch_repo::get_pattern_by_id(pool, grab.watch_pattern_id).await?
        else {
            continue;
        };
        let Some(want) = pattern_media_want(pool, &pattern).await else {
            continue;
        };
        let release = release_of(&Sighting {
            release_key: grab.release_guid.clone(),
            title: grab.release_title.clone(),
            size: None,
            published_at: None,
            indexer_source_id: None,
            indexer: String::new(),
            protocol: String::new(),
            download_url: None,
            magnet_url: None,
            info_hash: None,
        });
        let explanation = sport_explanation(&want, &release);
        let unexplained = explanation
            .as_ref()
            .map(|ex| slot_unexplained(&release.title, ex))
            .unwrap_or_default();
        let runner_up = runner_up(state, &rivals, &pattern, &want, &release).await;
        let close = runner_up
            .as_ref()
            .is_some_and(|r| grab.match_score - r.match_score < DOUBT_MARGIN);
        debug!(
            release = %release.title,
            unexplained = ?unexplained,
            runner_up = ?runner_up,
            "Checked grab for doubt"
        );
        if unexplained.is_empty() && !close {
            continue;
        }
        let detail = Detail {
            want: want_label(&want),
            media_kind: crate::media::parse_media_kind(&want).to_string(),
            due_at: None,
            candidates: Vec::new(),
            grab: Some(Grab {
                watch_match_id: grab.id,
                release_key: grab.release_guid,
                suggested_alias: suggested_alias(&want, &release.title, explanation.as_ref()),
                title: grab.release_title,
                match_score: grab.match_score,
                matched_pattern: grab.matched_pattern,
                explanation,
                unexplained,
                runner_up,
            }),
        };
        if repo::insert_item(
            pool,
            pattern.id,
            ItemKind::Doubtful.as_str(),
            &detail.to_json(),
        )
        .await?
        .is_some()
        {
            created += 1;
        }
    }
    let until = now
        .format(&time::format_description::well_known::Rfc3339)
        .unwrap_or_default();
    app_settings::set(pool, CHECKED_UNTIL, &serde_json::json!(until)).await?;
    Ok(created)
}

/// The other want of the same kind that takes this release most readily.
async fn runner_up(
    state: &AppState,
    rivals: &[Rival],
    pattern: &WatchPatternRow,
    want: &MediaWant,
    release: &crate::watch::Release,
) -> Option<RunnerUp> {
    let kind = crate::media::parse_media_kind(want);
    let mut best: Option<RunnerUp> = None;
    for rival in rivals.iter().filter(|r| {
        r.pattern.id != pattern.id
            && r.kind == kind
            && best_similarity(&r.names, &release.title) >= RELATED
    }) {
        let scored = score_for_pattern(state, &rival.pattern, std::slice::from_ref(release)).await;
        // Identity only: a rival that would turn the release down for its
        // quality or size still names the same thing.
        if let Some(score) = scored
            .iter()
            .filter(|pr| {
                !pr.structural_rejected
                    && !pr
                        .reject_reasons
                        .iter()
                        .any(|r| matches!(r, nats_jobs::RejectReason::EpisodeNotNeeded))
            })
            .filter_map(|pr| pr.match_score)
            .reduce(f64::max)
            && best.as_ref().is_none_or(|b| score > b.match_score)
        {
            best = Some(RunnerUp {
                watch_pattern_id: rival.pattern.id,
                want: rival.label.clone(),
                match_score: score,
            });
        }
    }
    best
}
