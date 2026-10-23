//! What the user can do with a review item.

use nats_jobs::MediaWant;
use uuid::Uuid;

use super::{Detail, ItemKind, release_of};
use crate::learned_aliases::{AliasScope, RememberError, remember};
use crate::repositories::RepositoryError;
use crate::repositories::learned_aliases::LearnedAliasRow;
use crate::repositories::review::{self as repo, ReviewItemRow};
use crate::repositories::watch::{self as watch_repo, WatchPatternRow};
use crate::state::AppState;

#[derive(Debug, thiserror::Error)]
pub enum ActionError {
    #[error("review item not found")]
    NotFound,
    #[error("the item is already closed")]
    Closed,
    #[error("no such release on this item")]
    UnknownRelease,
    #[error("the download could not be created")]
    GrabFailed,
    #[error(transparent)]
    Remember(#[from] RememberError),
    #[error(transparent)]
    Repository(#[from] RepositoryError),
}

pub struct OpenItem {
    pub row: ReviewItemRow,
    pub detail: Detail,
    pub pattern: WatchPatternRow,
}

async fn open_item(state: &AppState, id: Uuid) -> Result<OpenItem, ActionError> {
    let row = repo::get_item(&state.pool, id)
        .await?
        .ok_or(ActionError::NotFound)?;
    if row.status != "open" {
        return Err(ActionError::Closed);
    }
    let detail: Detail =
        serde_json::from_value(row.detail.clone()).map_err(|_| ActionError::NotFound)?;
    let pattern = watch_repo::get_pattern_by_id(&state.pool, row.watch_pattern_id)
        .await?
        .ok_or(ActionError::NotFound)?;
    Ok(OpenItem {
        row,
        detail,
        pattern,
    })
}

/// Grab one of a missed item's candidates for its want, and close the item.
pub async fn grab(state: &AppState, id: Uuid, release_key: &str) -> Result<Uuid, ActionError> {
    let item = open_item(state, id).await?;
    let candidate = item
        .detail
        .candidates
        .iter()
        .find(|c| c.sighting.release_key == release_key)
        .ok_or(ActionError::UnknownRelease)?;
    let download_id =
        crate::watch::grab_reviewed_release(state, &item.pattern, &release_of(&candidate.sighting))
            .await
            .ok_or(ActionError::GrabFailed)?;
    repo::close(&state.pool, id, "resolved").await?;
    Ok(download_id)
}

/// Record that a release is not this want. A missed item drops the
/// candidate and closes once none are left; a doubtful item's grab is
/// rejected, which cancels its download if it is still running.
pub async fn not_it(state: &AppState, id: Uuid, release_key: &str) -> Result<(), ActionError> {
    let mut item = open_item(state, id).await?;
    let pool = &state.pool;
    match item.row.kind.as_str() {
        k if k == ItemKind::Doubtful.as_str() => {
            let grab = item
                .detail
                .grab
                .as_ref()
                .filter(|g| g.release_key == release_key)
                .ok_or(ActionError::UnknownRelease)?;
            repo::reject(pool, item.pattern.id, release_key).await?;
            watch_repo::reject_match(pool, grab.watch_match_id, Some("not it (review)")).await?;
            repo::close(pool, id, "resolved").await?;
        }
        _ => {
            let before = item.detail.candidates.len();
            item.detail
                .candidates
                .retain(|c| c.sighting.release_key != release_key);
            if item.detail.candidates.len() == before {
                return Err(ActionError::UnknownRelease);
            }
            repo::reject(pool, item.pattern.id, release_key).await?;
            if item.detail.candidates.is_empty() {
                repo::close(pool, id, "resolved").await?;
            } else {
                repo::update_detail(pool, id, &item.detail.to_json()).await?;
            }
        }
    }
    Ok(())
}

/// Remember a name for the item's want, at the scope the user chose. The
/// item stays open: the user may still want to grab or dismiss it.
pub async fn remember_name(
    state: &AppState,
    id: Uuid,
    scope: AliasScope,
    alias: &str,
) -> Result<LearnedAliasRow, ActionError> {
    let item = open_item(state, id).await?;
    let want: MediaWant = item
        .pattern
        .media_want
        .clone()
        .and_then(|v| serde_json::from_value(v).ok())
        .ok_or(ActionError::NotFound)?;
    Ok(remember(&state.pool, &want, scope, alias).await?)
}

pub async fn dismiss(state: &AppState, id: Uuid) -> Result<(), ActionError> {
    match repo::close(&state.pool, id, "dismissed").await? {
        true => Ok(()),
        false => Err(ActionError::NotFound),
    }
}
