use std::time::Duration;

use tokio::task::JoinHandle;
use tracing::warn;

use super::indexer_search::{self, is_usenet_source};
use super::service::StreamSearchContext;
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
        let releases: Vec<Release> = match indexer_search::search(
            &pool,
            &http_client,
            indexers.as_ref(),
            &request,
            Some(is_usenet_source),
        )
        .await
        {
            Ok(found) => found,
            Err(e) => {
                warn!(error = %e, "Usenet indexer search failed");
                Vec::new()
            }
        };
        indexer_search::remember(&key, &releases);
        releases
    }))
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
