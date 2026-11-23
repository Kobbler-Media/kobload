use std::collections::{BTreeMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sqlx::PgPool;
use tokio::task::JoinHandle;
use tracing::warn;
use uuid::Uuid;

use super::indexer_search::{self, TvScope, is_episode_want, is_usenet_source};
use super::service::StreamSearchContext;
use crate::indexers::IndexerResolver;
use crate::watch::Release;
use nats_jobs::types::StreamSearchRequest;

pub const DEADLINE: Duration = Duration::from_secs(4);

pub enum UsenetSearch {
    Cached(Vec<Release>),
    Running {
        handle: JoinHandle<Vec<Release>>,
        answers: Arc<Mutex<Answers>>,
    },
}

pub struct Collected {
    pub releases: Vec<Release>,
    pub status: &'static str,
}

// Keyed by source position so a snapshot merges in the order the complete result would,
// whichever indexer happened to answer first.
#[derive(Default)]
pub struct Answers {
    wanted: BTreeMap<usize, Vec<Release>>,
    season: BTreeMap<usize, Vec<Release>>,
}

impl Answers {
    fn record(&mut self, scope: TvScope, source: usize, releases: &[Release]) {
        let answered = match scope {
            TvScope::AsWanted => &mut self.wanted,
            TvScope::Season => &mut self.season,
        };
        answered.insert(source, releases.to_vec());
    }

    fn is_empty(&self) -> bool {
        self.wanted.is_empty() && self.season.is_empty()
    }

    fn snapshot(&self) -> Vec<Release> {
        merge(
            self.wanted.values().flatten().cloned().collect(),
            self.season.values().flatten().cloned().collect(),
        )
    }
}

fn merge(mut wanted: Vec<Release>, season: Vec<Release>) -> Vec<Release> {
    let mut seen: HashSet<_> = wanted.iter().map(listing_id).collect();
    wanted.extend(season.into_iter().filter(|r| seen.insert(listing_id(r))));
    wanted
}

pub fn start(ctx: &StreamSearchContext<'_>, request: &StreamSearchRequest) -> UsenetSearch {
    let key = indexer_search::cache_key(request, "usenet");
    if let Some(releases) = indexer_search::cached(&key) {
        return UsenetSearch::Cached(releases);
    }
    let pool = ctx.pool.clone();
    let http_client = ctx.http_client.clone();
    let indexers = ctx.indexers.clone();
    let request = request.clone();
    let answers = Arc::new(Mutex::new(Answers::default()));
    let task_answers = answers.clone();
    // Detached rather than a job: a search cut at the deadline still finishes here and
    // fills the cache, so the next search for the same want lists its releases.
    let handle = tokio::spawn(async move {
        let wanted = search(
            &pool,
            &http_client,
            indexers.as_ref(),
            &request,
            TvScope::AsWanted,
            &task_answers,
        );
        // A newznab `ep` search lists only that episode's posts, never its season pack.
        let season = async {
            if is_episode_want(&request) {
                search(
                    &pool,
                    &http_client,
                    indexers.as_ref(),
                    &request,
                    TvScope::Season,
                    &task_answers,
                )
                .await
            } else {
                Vec::new()
            }
        };
        let (wanted, season) = tokio::join!(wanted, season);
        let releases = merge(wanted, season);
        indexer_search::remember(&key, &releases);
        releases
    });
    UsenetSearch::Running { handle, answers }
}

fn listing_id(release: &Release) -> (Option<Uuid>, String) {
    (release.indexer_source_id, release.guid.clone())
}

async fn search(
    pool: &PgPool,
    http_client: &reqwest::Client,
    indexers: &dyn IndexerResolver,
    request: &StreamSearchRequest,
    scope: TvScope,
    answers: &Mutex<Answers>,
) -> Vec<Release> {
    let on_source_answered = |source: usize, releases: &[Release]| {
        answers.lock().unwrap().record(scope, source, releases);
    };
    indexer_search::search(
        pool,
        http_client,
        indexers,
        request,
        Some(is_usenet_source),
        scope,
        Some(&on_source_answered),
    )
    .await
    .unwrap_or_else(|e| {
        warn!(error = %e, "Usenet indexer search failed");
        Vec::new()
    })
}

impl UsenetSearch {
    pub async fn collect(self, wait: Option<Duration>) -> Collected {
        let (handle, answers) = match self {
            Self::Cached(releases) => {
                return Collected {
                    releases,
                    status: "cached",
                };
            }
            Self::Running { handle, answers } => (handle, answers),
        };
        let joined = match wait {
            Some(wait) => match tokio::time::timeout(wait, handle).await {
                Ok(joined) => joined,
                Err(_) => return at_deadline(&answers.lock().unwrap()),
            },
            None => handle.await,
        };
        Collected {
            releases: joined.unwrap_or_default(),
            status: "success",
        }
    }
}

fn at_deadline(answers: &Answers) -> Collected {
    Collected {
        releases: answers.snapshot(),
        status: if answers.is_empty() {
            "deadline"
        } else {
            "partial"
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn release(guid: &str) -> Release {
        serde_json::from_value(json!({
            "guid": guid,
            "title": guid,
            "size": 1,
            "publish_date": "2026-10-01",
            "category_ids": [5000],
            "indexer": "test",
            "protocol": "usenet",
            "raw_data": {},
        }))
        .unwrap()
    }

    #[test]
    fn test_deadline_status_and_merge_order() {
        type Answer = (TvScope, usize, &'static [&'static str]);
        let cases: [(&[Answer], &str, &[&str]); 4] = [
            (&[], "deadline", &[]),
            (&[(TvScope::AsWanted, 1, &[])], "partial", &[]),
            (&[(TvScope::Season, 0, &["pack"])], "partial", &["pack"]),
            (
                &[
                    (TvScope::Season, 0, &["ep", "pack"]),
                    (TvScope::AsWanted, 1, &["later"]),
                    (TvScope::AsWanted, 0, &["ep"]),
                ],
                "partial",
                &["ep", "later", "pack"],
            ),
        ];
        for (recorded, status, guids) in cases {
            let mut answers = Answers::default();
            for (scope, source, found) in recorded {
                let found: Vec<_> = found.iter().map(|g| release(g)).collect();
                answers.record(*scope, *source, &found);
            }
            let collected = at_deadline(&answers);
            let got: Vec<_> = collected.releases.iter().map(|r| r.guid.as_str()).collect();
            assert_eq!(collected.status, status);
            assert_eq!(got, guids);
        }
    }
}
