use std::collections::HashSet;

use axum::http::StatusCode;
use tracing::{debug, warn};

use crate::format;
use crate::helpers::resolve_categories_for_source;
use crate::indexers::types::SearchType;
use crate::indexers::{IndexerContext, IndexerSearchRequest};
use crate::media::{search_terms_from_media_want, season_episode_from_media_want};
use crate::prowlarr::ProwlarrCategory;
use crate::repositories::search_cache;
use crate::repositories::watch as watch_repo;
use crate::types::{Category, SearchResult, TorrentStats, UsenetStats};
use crate::watch::Release;

use super::types::{
    SearchServiceContext, SearchServiceError, SearchServiceRequest, SearchServiceResult,
};

/// Default TTL for cached search results (5 minutes)
const CACHE_TTL_SECONDS: i64 = 300;

/// Optional callback for quality-aware early exit. Called after each source's
/// results are collected. If it returns `true`, remaining sources are skipped.
pub type EarlyExitCheck<'a> = &'a (dyn Fn(&[SearchResult]) -> bool + Send + Sync);

pub async fn search(
    ctx: &SearchServiceContext<'_>,
    request: SearchServiceRequest,
    collector: Option<&crate::decision_log::DecisionCollector>,
    early_exit_check: Option<EarlyExitCheck<'_>>,
) -> Result<SearchServiceResult, SearchServiceError> {
    // Build cache key from request parameters
    let cache_key = search_cache::build_cache_key(
        request.media_type_id,
        &request.query,
        &[], // We don't have category IDs at request level
        request.limit,
        request.offset,
        request.media_want.as_ref(),
    );

    // Check cache first
    if let Ok(Some(cached_json)) = search_cache::get(ctx.pool, &cache_key).await
        && let Ok(cached_result) = serde_json::from_value::<SearchServiceResult>(cached_json)
    {
        debug!(cache_key = %cache_key, "Search cache hit");
        return Ok(cached_result);
    }

    let queries = if request.query.trim().is_empty() {
        let Some(ref media_want) = request.media_want else {
            return Err(SearchServiceError::new(
                StatusCode::BAD_REQUEST,
                "query or media_want required",
            ));
        };
        let terms = search_terms_from_media_want(media_want);
        if terms.is_empty() {
            return Err(SearchServiceError::new(
                StatusCode::BAD_REQUEST,
                "media_want did not produce search terms",
            ));
        }
        terms
    } else {
        vec![request.query.clone()]
    };

    // Record queries in decision log
    if let Some(log) = collector {
        for q in &queries {
            log.record_query(q);
        }
    }

    let search_type = search_type_from_media_want(request.media_want.as_ref());

    let (season, episode) = request
        .media_want
        .as_ref()
        .map(season_episode_from_media_want)
        .unwrap_or((None, None));

    let indexer_ctx = IndexerContext {
        pool: ctx.pool,
        http_client: ctx.http_client,
    };

    let enabled_sources = watch_repo::list_enabled_sources(ctx.pool)
        .await
        .map_err(|e| SearchServiceError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;

    // Filter sources by media_type_id (empty media_type_ids = serves all)
    let matching_sources: Vec<&watch_repo::IndexerSourceRow> = enabled_sources
        .iter()
        .filter(|source| {
            if let Some(mt_id) = request.media_type_id {
                source.media_type_ids.is_empty() || source.media_type_ids.contains(&mt_id)
            } else {
                true
            }
        })
        .collect();

    // Filter to sources with a search-capable handler
    let searchable_sources: Vec<&&watch_repo::IndexerSourceRow> = matching_sources
        .iter()
        .filter(|s| {
            ctx.indexers
                .get(&s.handler_type)
                .is_some_and(|h| h.capabilities().supports_search)
        })
        .collect();

    let mut results = Vec::new();
    let mut errors = Vec::new();
    let mut seen = HashSet::new();

    if searchable_sources.is_empty() {
        errors.push(
            "No indexer sources configured. Add sources in Settings → Indexer Sources.".to_string(),
        );
        return Ok(SearchServiceResult { results, errors });
    }

    for source in &searchable_sources {
        if watch_repo::should_skip_for_search(source, &request.search_context) {
            warn!(source = %source.name, "Skipping source due to rate limit, backoff, or throttle");
            if let Some(log) = collector {
                log.record_provider_call(crate::decision_log::LogProviderCall {
                    id: uuid::Uuid::new_v4(),
                    indexer_source_id: Some(source.id),
                    source_name: source.name.clone(),
                    handler_type: source.handler_type.clone(),
                    search_type: None,
                    query: None,
                    categories: None,
                    external_ids: None,
                    results_count: 0,
                    results_after_dedup: 0,
                    status: "skipped".to_string(),
                    error_message: Some("Rate limited, backoff, or throttle".to_string()),
                    duration_ms: None,
                });
            }
            continue;
        }

        let handler = ctx.indexers.get(&source.handler_type).unwrap();

        let search_categories =
            resolve_categories_for_source(ctx.pool, source, request.media_type_id)
                .await
                .map_err(|e| SearchServiceError::new(StatusCode::BAD_REQUEST, e))?;

        let search_type = crate::indexers::types::resolve_search_type(source, search_type);

        // Try structured search first (IDs + season/ep, no free-text) if applicable
        let mut structured_hit = false;
        if !request.external_ids.is_empty()
            && super::indexers::can_do_structured_search(source, search_type, &request.external_ids)
        {
            let filtered_ids =
                super::indexers::filter_ids_by_caps(source, search_type, &request.external_ids);
            if !filtered_ids.is_empty() {
                let call_id = collector.map(|log| {
                    log.record_provider_call(crate::decision_log::LogProviderCall {
                        id: uuid::Uuid::new_v4(),
                        indexer_source_id: Some(source.id),
                        source_name: source.name.clone(),
                        handler_type: source.handler_type.clone(),
                        search_type: Some("structured".to_string()),
                        query: None,
                        categories: serde_json::to_value(&search_categories).ok(),
                        external_ids: serde_json::to_value(&filtered_ids).ok(),
                        results_count: 0,
                        results_after_dedup: 0,
                        status: "pending".to_string(),
                        error_message: None,
                        duration_ms: None,
                    })
                });
                let call_start = std::time::Instant::now();

                let search_request = IndexerSearchRequest {
                    query: String::new(),
                    categories: search_categories.clone(),
                    limit: None, // Don't send limit — let indexer use its default (matches Sonarr)
                    offset: request.offset,
                    external_ids: filtered_ids,
                    search_type,
                    season: season.clone(),
                    episode: episode.clone(),
                };

                let search_result = handler
                    .search(&indexer_ctx, Some(source), search_request)
                    .await;

                let _ = watch_repo::increment_api_calls(ctx.pool, source.id).await;

                match search_result {
                    Ok(releases) if !releases.is_empty() => {
                        let _ = watch_repo::record_success(ctx.pool, source.id).await;
                        debug!(source = %source.name, count = releases.len(), "Structured search hit");
                        let before = results.len();
                        collect_releases(releases, handler.handler_type(), &mut results, &mut seen);
                        let after_dedup = results.len() - before;
                        if let Some(log) = collector
                            && let Some(cid) = call_id
                        {
                            log.update_provider_call(
                                cid,
                                (results.len() - before + (after_dedup)) as i32,
                                after_dedup as i32,
                                "success",
                                None,
                                Some(call_start.elapsed().as_millis() as i32),
                            );
                        }
                        structured_hit = true;
                    }
                    Ok(_) => {
                        let _ = watch_repo::record_success(ctx.pool, source.id).await;
                        if let Some(log) = collector
                            && let Some(cid) = call_id
                        {
                            log.update_provider_call(
                                cid,
                                0,
                                0,
                                "success",
                                None,
                                Some(call_start.elapsed().as_millis() as i32),
                            );
                        }
                    }
                    Err(err) if err.status == StatusCode::TOO_MANY_REQUESTS => {
                        let _ = watch_repo::record_rate_limited(
                            ctx.pool,
                            source.id,
                            err.retry_after_seconds,
                        )
                        .await;
                        warn!(source = %source.name, "Indexer rate limited (429) on structured search");
                        errors.push(format!("Indexer '{}' rate limited", source.name));
                        if let Some(log) = collector
                            && let Some(cid) = call_id
                        {
                            log.update_provider_call(
                                cid,
                                0,
                                0,
                                "rate_limited",
                                Some(err.to_string()),
                                Some(call_start.elapsed().as_millis() as i32),
                            );
                        }
                        continue; // Skip text fallback for this source
                    }
                    Err(err) => {
                        crate::indexers::record_error(ctx.pool, source.id, &err).await;
                        warn!(error = %err, source = %source.name, "Structured search failed");
                        if let Some(log) = collector
                            && let Some(cid) = call_id
                        {
                            log.update_provider_call(
                                cid,
                                0,
                                0,
                                "error",
                                Some(err.to_string()),
                                Some(call_start.elapsed().as_millis() as i32),
                            );
                        }
                        // Fall through to text cascade
                    }
                }
            }
        }

        // Text cascade fallback (skip if structured search already returned results)
        // Also skip text fallback for TV wants with future or very recent air dates —
        // text search has no TVDB ID filtering and can match the wrong show.
        let text_fallback_blocked = request.media_want.as_ref().is_some_and(|mw| {
            let today = time::OffsetDateTime::now_utc().date();
            let leniency =
                time::Duration::days(crate::services::want::scoring::AIR_DATE_LENIENCY_DAYS);
            match mw {
                nats_jobs::MediaWant::TvEpisode {
                    air_date: Some(ad), ..
                } => crate::services::want::scoring::parse_iso_date(ad)
                    .is_some_and(|d| d - leniency > today),
                _ => false,
            }
        });

        if !structured_hit && !text_fallback_blocked {
            for query in &queries {
                let call_id = collector.map(|log| {
                    log.record_provider_call(crate::decision_log::LogProviderCall {
                        id: uuid::Uuid::new_v4(),
                        indexer_source_id: Some(source.id),
                        source_name: source.name.clone(),
                        handler_type: source.handler_type.clone(),
                        search_type: Some("text".to_string()),
                        query: Some(query.clone()),
                        categories: serde_json::to_value(&search_categories).ok(),
                        external_ids: None,
                        results_count: 0,
                        results_after_dedup: 0,
                        status: "pending".to_string(),
                        error_message: None,
                        duration_ms: None,
                    })
                });
                let call_start = std::time::Instant::now();

                let search_request = IndexerSearchRequest {
                    query: query.clone(),
                    categories: search_categories.clone(),
                    limit: None, // Don't send limit — let indexer use its default (matches Sonarr)
                    offset: request.offset,
                    external_ids: Vec::new(),
                    search_type,
                    season: None,
                    episode: None,
                };

                let search_result = handler
                    .search(&indexer_ctx, Some(source), search_request)
                    .await;

                let _ = watch_repo::increment_api_calls(ctx.pool, source.id).await;

                match search_result {
                    Ok(releases) => {
                        let _ = watch_repo::record_success(ctx.pool, source.id).await;
                        let count = releases.len();
                        let before = results.len();
                        collect_releases(releases, handler.handler_type(), &mut results, &mut seen);
                        let after_dedup = results.len() - before;
                        if let Some(log) = collector
                            && let Some(cid) = call_id
                        {
                            log.update_provider_call(
                                cid,
                                count as i32,
                                after_dedup as i32,
                                "success",
                                None,
                                Some(call_start.elapsed().as_millis() as i32),
                            );
                        }
                        if count > 0 {
                            break; // Got results, stop cascade for this source
                        }
                    }
                    Err(err) if err.status == StatusCode::TOO_MANY_REQUESTS => {
                        let _ = watch_repo::record_rate_limited(
                            ctx.pool,
                            source.id,
                            err.retry_after_seconds,
                        )
                        .await;
                        warn!(source = %source.name, "Indexer rate limited (429)");
                        errors.push(format!("Indexer '{}' rate limited", source.name));
                        if let Some(log) = collector
                            && let Some(cid) = call_id
                        {
                            log.update_provider_call(
                                cid,
                                0,
                                0,
                                "rate_limited",
                                Some(err.to_string()),
                                Some(call_start.elapsed().as_millis() as i32),
                            );
                        }
                        break;
                    }
                    Err(err) => {
                        crate::indexers::record_error(ctx.pool, source.id, &err).await;
                        warn!(error = %err, source = %source.name, "Indexer search failed");
                        errors.push(format!("Indexer '{}' search failed: {}", source.name, err));
                        if let Some(log) = collector
                            && let Some(cid) = call_id
                        {
                            log.update_provider_call(
                                cid,
                                0,
                                0,
                                "error",
                                Some(err.to_string()),
                                Some(call_start.elapsed().as_millis() as i32),
                            );
                        }

                        if err.status == StatusCode::BAD_REQUEST
                            || err.status == StatusCode::INTERNAL_SERVER_ERROR
                        {
                            return Err(SearchServiceError::new(err.status, err.message));
                        }
                        break;
                    }
                }
            }
        }

        // Quality-aware early exit: if accumulated results already contain a
        // satisfactory match, skip remaining sources.
        if let Some(check) = early_exit_check
            && check(&results)
        {
            debug!(
                source = %source.name,
                total_results = results.len(),
                "Early exit: satisfactory results found, skipping remaining sources"
            );
            break;
        }
    }

    let result = SearchServiceResult { results, errors };

    // Store in cache (only if we have results)
    if !result.results.is_empty()
        && let Ok(results_json) = serde_json::to_value(&result)
    {
        if let Err(e) =
            search_cache::put(ctx.pool, &cache_key, &results_json, CACHE_TTL_SECONDS).await
        {
            warn!(cache_key = %cache_key, error = %e, "Failed to cache search results");
        } else {
            debug!(cache_key = %cache_key, count = result.results.len(), "Cached search results");
        }
    }

    Ok(result)
}

fn release_dedup_key(release: &Release) -> String {
    if !release.guid.is_empty() {
        format!("guid:{}", release.guid)
    } else if let Some(url) = &release.download_url {
        format!("download:{}", url)
    } else if let Some(url) = &release.magnet_url {
        format!("magnet:{}", url)
    } else {
        format!("title:{}", release.title)
    }
}

fn collect_releases(
    releases: Vec<Release>,
    indexer_type: &str,
    results: &mut Vec<SearchResult>,
    seen: &mut HashSet<String>,
) {
    for release in releases {
        let id = release_dedup_key(&release);
        if seen.contains(&id) {
            continue;
        }
        seen.insert(id.clone());
        results.push(build_search_result(id, release, indexer_type));
    }
}

fn build_search_result(id: String, release: Release, indexer_type: &str) -> SearchResult {
    let protocol = release.protocol.clone();
    let categories = extract_categories(&release);

    let (torrent_stats, usenet_stats) = match protocol.as_str() {
        "torrent" => (
            Some(TorrentStats {
                seeders: release.seeders,
                peers: release.peers,
                leechers: release.leechers,
            }),
            None,
        ),
        "usenet" => (
            None,
            Some(UsenetStats {
                grabs: release.grabs,
            }),
        ),
        _ => (None, None),
    };

    SearchResult {
        id,
        guid: Some(release.guid),

        title: release.title.clone(),
        file_name: release.file_name.clone(),
        format: format::detect_format_from_text(release.file_name.as_deref(), &release.title),
        size: release.size,
        files: release.files,

        indexer: release.indexer,
        indexer_type: indexer_type.to_string(),
        indexer_id: release.indexer_id.map(|id| id.to_string()),
        info_url: release.info_url,

        protocol,
        download_url: release.download_url,
        magnet_url: release.magnet_url,
        info_hash: release.info_hash,

        publish_date: Some(release.publish_date),
        torrent: torrent_stats,
        usenet: usenet_stats,

        media_type: None,
        categories,
    }
}

fn extract_categories(release: &Release) -> Option<Vec<Category>> {
    if !release.category_ids.is_empty() {
        let cats: Vec<Category> = release
            .category_ids
            .iter()
            .map(|&id| Category {
                id,
                name: None,
                sub_categories: Vec::new(),
            })
            .collect();
        return Some(cats);
    }

    let categories_value = release.raw_data.get("categories")?.clone();
    let categories: Vec<ProwlarrCategory> = serde_json::from_value(categories_value).ok()?;
    Some(categories.into_iter().map(convert_category).collect())
}

fn convert_category(cat: ProwlarrCategory) -> Category {
    Category {
        id: cat.id,
        name: cat.name,
        sub_categories: cat
            .sub_categories
            .into_iter()
            .map(convert_category)
            .collect(),
    }
}

pub fn search_type_from_media_want(media_want: Option<&nats_jobs::MediaWant>) -> SearchType {
    match media_want {
        Some(nats_jobs::MediaWant::Book { .. } | nats_jobs::MediaWant::Audiobook { .. }) => {
            SearchType::BookSearch
        }
        Some(
            nats_jobs::MediaWant::TvEpisode { .. }
            | nats_jobs::MediaWant::TvSeason { .. }
            | nats_jobs::MediaWant::SportEvent { .. }
            | nats_jobs::MediaWant::SportSeason { .. },
        ) => SearchType::TvSearch,
        Some(nats_jobs::MediaWant::Movie { .. }) => SearchType::MovieSearch,
        Some(nats_jobs::MediaWant::Comic { .. }) => SearchType::BookSearch,
        Some(nats_jobs::MediaWant::Music { .. }) => SearchType::MusicSearch,
        Some(nats_jobs::MediaWant::Game { .. } | nats_jobs::MediaWant::Software { .. }) | None => {
            SearchType::Generic
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::repositories::watch::SearchContext;

    #[test]
    fn test_extract_categories_from_release() {
        let raw_data = serde_json::json!({
            "categories": [
                { "id": 7000, "name": "Books", "subCategories": [] },
                { "id": 7020, "name": "E-Books", "subCategories": [] }
            ]
        });

        let release = Release {
            guid: "guid-1".to_string(),
            title: "Test".to_string(),
            file_name: None,
            download_url: None,
            magnet_url: None,
            info_url: None,
            size: 123,
            publish_date: "2024-01-01T00:00:00Z".to_string(),
            category_ids: vec![7000, 7020],
            indexer: "Indexer".to_string(),
            protocol: "torrent".to_string(),
            info_hash: None,
            files: None,
            seeders: None,
            peers: None,
            leechers: None,
            grabs: None,
            votes: None,
            indexer_id: None,
            indexer_source_id: None,
            raw_data,
            external_ids: Vec::new(),
            media: None,
        };

        let categories = extract_categories(&release).expect("categories");
        let ids: Vec<i32> = categories.iter().map(|c| c.id).collect();
        assert_eq!(ids, vec![7000, 7020]);
    }

    #[tokio::test]
    async fn test_search_requires_query() {
        let pool =
            sqlx::PgPool::connect_lazy("postgres://postgres@localhost/kobload_test").expect("pool");
        let http_client = crate::http::new_client();
        let resolver = crate::indexers::DefaultIndexerResolver::new(Default::default());
        let ctx = SearchServiceContext {
            pool: &pool,
            http_client: &http_client,
            indexers: &resolver,
        };

        let request = SearchServiceRequest {
            query: "   ".to_string(),
            media_type_id: None,
            protocol: None,
            limit: None,
            offset: None,
            media_want: None,
            external_ids: Vec::new(),
            search_context: SearchContext::Interactive,
        };

        let err = search(&ctx, request, None, None).await.expect_err("error");
        assert_eq!(err.status, StatusCode::BAD_REQUEST);
    }
}
