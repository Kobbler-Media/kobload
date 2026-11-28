use std::sync::LazyLock;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use sqlx::PgPool;
use tracing::{info, warn};

use super::service::{
    build_patterns, build_search_query, load_quality_profile, media_want_external_ids,
    scoring_config,
};
use super::shared_lookup::SharedLookups;
use crate::indexers::{IndexerContext, IndexerResolver};
use crate::repositories::watch::IndexerSourceRow;
use crate::scene_titles::TvNaming;
use crate::services::search::{
    IndexerSearch, SearchIndexersParams, SourceAnswered, SourceFilter, search_indexers,
};
use crate::watch::{ExternalId, ExternalIdSource, ProcessedRelease, Release};
use nats_jobs::types::StreamSearchRequest;

const CACHE_TTL: Duration = Duration::from_secs(900);
const CACHE_SWEEP_ABOVE: usize = 100;

static RESULT_CACHE: LazyLock<DashMap<String, (Vec<Release>, Instant)>> =
    LazyLock::new(DashMap::new);

pub static FALLBACK: LazyLock<SharedLookups<IndexerSearch>> =
    LazyLock::new(|| SharedLookups::new(CACHE_TTL));

#[derive(Clone, Copy)]
pub enum TvScope {
    AsWanted,
    Season,
}

pub fn is_episode_want(request: &StreamSearchRequest) -> bool {
    matches!(
        request.media_want,
        Some(nats_jobs::MediaWant::TvEpisode { .. })
    )
}

pub fn is_usenet_source(source: &IndexerSourceRow) -> bool {
    source.handler_type == "newznab"
}

pub fn is_not_usenet_source(source: &IndexerSourceRow) -> bool {
    !is_usenet_source(source)
}

pub fn cache_key(request: &StreamSearchRequest, scope: &str) -> String {
    let want = request
        .media_want
        .as_ref()
        .and_then(|mw| serde_json::to_string(mw).ok())
        .unwrap_or_default();
    format!("{scope}|{}|{want}", request.query)
}

pub fn cached(key: &str) -> Option<Vec<Release>> {
    let fresh = RESULT_CACHE
        .get(key)
        .filter(|entry| entry.1.elapsed() < CACHE_TTL)
        .map(|entry| entry.0.clone());
    if fresh.is_none() {
        RESULT_CACHE.remove(key);
    }
    fresh
}

pub fn remember(key: &str, releases: &[Release]) {
    if releases.is_empty() {
        return;
    }
    RESULT_CACHE.insert(key.to_string(), (releases.to_vec(), Instant::now()));
    if RESULT_CACHE.len() > CACHE_SWEEP_ABOVE {
        RESULT_CACHE.retain(|_, (_, inserted_at)| inserted_at.elapsed() < CACHE_TTL);
    }
}

pub async fn search(
    pool: &PgPool,
    http_client: &reqwest::Client,
    indexers: &dyn IndexerResolver,
    request: &StreamSearchRequest,
    source_filter: Option<SourceFilter>,
    scope: TvScope,
    on_source_answered: Option<SourceAnswered<'_>>,
) -> Result<IndexerSearch, String> {
    let external_ids: Vec<ExternalId> = request
        .media_want
        .as_ref()
        .map(media_want_external_ids)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|(source, id)| {
            let source = match source.as_str() {
                "imdb" => ExternalIdSource::Imdb,
                "tmdb" => ExternalIdSource::Tmdb,
                "tvdb" => ExternalIdSource::Tvdb,
                "tvmaze" => ExternalIdSource::TvMaze,
                _ => return None,
            };
            Some(ExternalId { source, id })
        })
        .collect();

    let (season, episode) = match request.media_want.as_ref() {
        Some(nats_jobs::MediaWant::TvEpisode {
            season, episode, ..
        }) => (
            Some(season.clone()),
            matches!(scope, TvScope::AsWanted).then(|| episode.clone()),
        ),
        _ => (None, None),
    };

    let query = build_search_query(request);
    let ctx = IndexerContext { pool, http_client };
    search_indexers(&SearchIndexersParams {
        ctx: &ctx,
        indexers,
        pool,
        query: &query,
        limit: 100,
        external_ids: &external_ids,
        media_want: request.media_want.as_ref(),
        media_type_id: None,
        // 30s allows FlareSolverr time to solve Cloudflare challenges (~10-20s).
        per_source_timeout: Some(Duration::from_secs(30)),
        season,
        episode,
        source_filter,
        on_source_answered,
    })
    .await
}

/// Indexer results score without TRaSH filters: public trackers commonly carry unnamed
/// groups and HD x265, which TRaSH rejects.
pub async fn score(
    pool: &PgPool,
    request: &StreamSearchRequest,
    releases: &[Release],
    naming: &TvNaming,
) -> Vec<ProcessedRelease> {
    score_with(pool, request, releases, naming, false).await
}

/// A usenet season pack is offered for one of its episodes: the engine imports its layout
/// on resolve and streams only the wanted file.
pub async fn score_usenet(
    pool: &PgPool,
    request: &StreamSearchRequest,
    releases: &[Release],
    naming: &TvNaming,
) -> Vec<ProcessedRelease> {
    score_with(pool, request, releases, naming, true).await
}

async fn score_with(
    pool: &PgPool,
    request: &StreamSearchRequest,
    releases: &[Release],
    naming: &TvNaming,
    usenet_episode_packs: bool,
) -> Vec<ProcessedRelease> {
    if releases.is_empty() {
        return Vec::new();
    }
    let patterns = build_patterns(request, naming);
    let quality_profile = load_quality_profile(pool, request).await;
    let mut config = scoring_config(request, &patterns, quality_profile.as_ref(), None, naming);
    config.usenet_episode_packs = usenet_episode_packs;
    let scored = crate::services::want::scoring::score_batch(releases, &config);
    let candidate_count = scored.iter().filter(|pr| pr.is_candidate()).count();
    info!(
        release_count = releases.len(),
        candidate_count, "Scored indexer releases"
    );
    if candidate_count == 0 {
        warn!(
            release_count = releases.len(),
            "All indexer releases rejected during scoring — check patterns and quality profile"
        );
    }
    scored
}
