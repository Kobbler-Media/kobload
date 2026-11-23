use std::collections::HashSet;
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
    Running(JoinHandle<Vec<Release>>),
}

pub struct Collected {
    pub releases: Vec<Release>,
    pub status: &'static str,
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
    // Detached rather than a job: a search cut at the deadline still finishes here and
    // fills the cache, so the next search for the same want lists its releases.
    UsenetSearch::Running(tokio::spawn(async move {
        let wanted = search(
            &pool,
            &http_client,
            indexers.as_ref(),
            &request,
            TvScope::AsWanted,
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
                )
                .await
            } else {
                Vec::new()
            }
        };
        let (mut releases, season) = tokio::join!(wanted, season);
        let mut seen: HashSet<_> = releases.iter().map(listing_id).collect();
        releases.extend(season.into_iter().filter(|r| seen.insert(listing_id(r))));
        indexer_search::remember(&key, &releases);
        releases
    }))
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
) -> Vec<Release> {
    indexer_search::search(
        pool,
        http_client,
        indexers,
        request,
        Some(is_usenet_source),
        scope,
    )
    .await
    .unwrap_or_else(|e| {
        warn!(error = %e, "Usenet indexer search failed");
        Vec::new()
    })
}

impl UsenetSearch {
    pub async fn collect(self, wait: Option<Duration>) -> Collected {
        let handle = match self {
            Self::Cached(releases) => {
                return Collected {
                    releases,
                    status: "cached",
                };
            }
            Self::Running(handle) => handle,
        };
        let joined = match wait {
            Some(wait) => match tokio::time::timeout(wait, handle).await {
                Ok(joined) => joined,
                Err(_) => {
                    return Collected {
                        releases: Vec::new(),
                        status: "deadline",
                    };
                }
            },
            None => handle.await,
        };
        Collected {
            releases: joined.unwrap_or_default(),
            status: "success",
        }
    }
}
