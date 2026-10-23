//! Wants whose date passed a while ago with nothing grabbed, shown with the
//! releases seen since that look most like them.

use std::collections::HashSet;

use nats_jobs::MediaWant;
use time::{Duration, OffsetDateTime};
use uuid::Uuid;

use super::similarity::best_similarity;
use super::want::{contradicts_event, due_at, names, suggested_alias, want_label, words};
use super::{Candidate, Detail, ItemKind, release_of};
use crate::repositories::release_sightings::{self, Sighting};
use crate::repositories::{RepositoryError, hash_index, review as repo, watch as watch_repo};
use crate::services::want::scoring::sport_explanation;
use crate::state::AppState;

/// Releases routinely land hours after the event; this is past that.
const GRACE: Duration = Duration::hours(12);
/// Sightings are kept this long, so there is nothing to show past it.
const LOOKBACK: Duration = Duration::days(release_sightings::RETENTION_DAYS as i64);
/// Releases a little early (time zones, pre-air leaks) still belong.
const EARLY: Duration = Duration::days(1);
const MAX_CANDIDATES: usize = 5;
const MIN_SIMILARITY: f64 = 0.5;
const HASH_INDEX_LIMIT: i64 = 500;

pub(super) async fn generate(
    state: &AppState,
    now: OffsetDateTime,
) -> Result<usize, RepositoryError> {
    let pool = &state.pool;
    let existing = repo::items_by_pattern(pool).await?;
    let grabbed = repo::patterns_with_grab(pool).await?;
    let mut created = 0;
    for pattern in watch_repo::get_all_enabled_patterns(pool).await? {
        if !pattern.auto_download
            || grabbed.contains(&pattern.id)
            || existing.contains(&(pattern.id, ItemKind::Missed.as_str().to_string()))
        {
            continue;
        }
        let Some(due) = pattern
            .media_want
            .clone()
            .and_then(|v| serde_json::from_value::<MediaWant>(v).ok())
            .and_then(|w| due_at(&w))
            .filter(|d| *d <= now - GRACE && *d >= now - LOOKBACK)
        else {
            continue;
        };
        let Some(want) = crate::watch::pattern_media_want(pool, &pattern).await else {
            continue;
        };
        let candidates = nearest(state, &want, pattern.id, due - EARLY).await?;
        if candidates.is_empty() {
            continue;
        }
        let detail = Detail {
            want: want_label(&want),
            media_kind: crate::media::parse_media_kind(&want).to_string(),
            due_at: Some(due),
            candidates,
            grab: None,
        };
        if repo::insert_item(
            pool,
            pattern.id,
            ItemKind::Missed.as_str(),
            &detail.to_json(),
        )
        .await?
        .is_some()
        {
            created += 1;
        }
    }
    Ok(created)
}

/// The releases seen since `since` most like the want: date window and, for
/// sport, class and season are hard filters; name similarity ranks the rest.
async fn nearest(
    state: &AppState,
    want: &MediaWant,
    pattern_id: Uuid,
    since: OffsetDateTime,
) -> Result<Vec<Candidate>, RepositoryError> {
    let pool = &state.pool;
    let names = names(want);
    let words = words(&names);
    let rejected = repo::rejected_keys(pool, pattern_id).await?;
    let mut seen: HashSet<String> = HashSet::new();
    let sightings = release_sightings::seen_with_any_word(pool, since, &words)
        .await?
        .into_iter()
        .chain(
            hash_index::recent_with_any_word(pool, since, &words, HASH_INDEX_LIMIT)
                .await?
                .into_iter()
                .map(|e| Sighting {
                    release_key: e.info_hash.clone(),
                    title: e.original_title,
                    size: e.size,
                    published_at: Some(e.first_seen),
                    indexer_source_id: None,
                    indexer: "Hash Index".to_string(),
                    protocol: "torrent".to_string(),
                    download_url: None,
                    magnet_url: e.magnet_url,
                    info_hash: Some(e.info_hash),
                }),
        );
    let mut candidates: Vec<Candidate> = sightings
        .filter(|s| !rejected.contains(&s.release_key) && seen.insert(s.release_key.clone()))
        .filter(|s| s.published_at.is_none_or(|p| p >= since))
        .filter_map(|s| {
            let explanation = sport_explanation(want, &release_of(&s));
            if explanation.as_ref().is_some_and(contradicts_event) {
                return None;
            }
            let similarity = best_similarity(&names, &s.title);
            (similarity >= MIN_SIMILARITY).then(|| Candidate {
                suggested_alias: suggested_alias(want, &s.title, explanation.as_ref()),
                similarity,
                explanation,
                sighting: s,
            })
        })
        .collect();
    candidates.sort_by(|a, b| {
        b.similarity
            .total_cmp(&a.similarity)
            .then_with(|| a.sighting.title.cmp(&b.sighting.title))
    });
    candidates.truncate(MAX_CANDIDATES);
    Ok(candidates)
}
