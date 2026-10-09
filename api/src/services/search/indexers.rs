//! Shared indexer search logic.
//!
//! Searches all enabled indexer sources with structured→fallback cascade.
//! For Newznab sources with stored capabilities, tries ID-based structured search
//! first (no free-text query), then falls back to the text-based cascade.

use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::helpers::resolve_categories_for_source;
use crate::indexers::types::{NewznabCaps, SearchType, resolve_search_type};
use crate::indexers::{IndexerContext, IndexerResolver, IndexerSearchRequest};
use crate::repositories::watch::{self as watch_repo, SearchContext};
use crate::services::search::search_type_from_media_want;
use crate::watch::{ExternalId, ExternalIdSource, Release};

pub struct SearchIndexersParams<'a> {
    pub ctx: &'a IndexerContext<'a>,
    pub indexers: &'a dyn IndexerResolver,
    pub pool: &'a sqlx::PgPool,
    pub query: &'a str,
    pub limit: u32,
    pub external_ids: &'a [ExternalId],
    pub media_want: Option<&'a nats_jobs::MediaWant>,
    pub media_type_id: Option<Uuid>,
    /// Per-source timeout. If set, each indexer search is wrapped in this timeout.
    /// Sources that exceed the timeout are skipped (treated as a failure).
    pub per_source_timeout: Option<std::time::Duration>,
    /// Season for structured TV search (Newznab/Cardigann `season` param).
    pub season: Option<String>,
    /// Episode for structured TV search (Newznab/Cardigann `ep` param).
    pub episode: Option<String>,
}

pub async fn search_indexers(params: &SearchIndexersParams<'_>) -> Result<Vec<Release>, String> {
    let ctx = params.ctx;
    let indexers = params.indexers;
    let pool = params.pool;
    let query = params.query;
    let _limit = params.limit;
    let external_ids = params.external_ids;
    let media_want = params.media_want;
    let media_type_id = params.media_type_id;

    let search_type = search_type_from_media_want(media_want);

    let enabled_sources = watch_repo::list_enabled_sources(pool)
        .await
        .map_err(|e| e.to_string())?;

    let searchable_sources: Vec<_> = enabled_sources
        .iter()
        .filter(|s| {
            if let Some(mt_id) = media_type_id {
                s.media_type_ids.is_empty() || s.media_type_ids.contains(&mt_id)
            } else {
                true
            }
        })
        .collect();

    if searchable_sources.is_empty() {
        warn!("No indexer sources configured");
        return Err(
            "No indexer sources configured. Add sources in Settings → Indexer Sources.".to_string(),
        );
    }

    let mut releases = Vec::new();

    for source in &searchable_sources {
        if watch_repo::should_skip_source(source) {
            warn!(source = %source.name, "Skipping source due to rate limit or backoff");
            continue;
        }

        let handler = match indexers.get(&source.handler_type) {
            Some(h) => h,
            None => {
                // Skip handler types that don't support search (e.g. prowlarr_rss)
                continue;
            }
        };

        if !handler.capabilities().supports_search {
            continue;
        }

        let search_categories = resolve_categories_for_source(pool, source, media_type_id)
            .await
            .map_err(|e| format!("Category resolution failed: {e}"))?;

        let search_type = resolve_search_type(source, search_type);

        let search_fut = handler.search(
            ctx,
            Some(source),
            IndexerSearchRequest {
                query: query.to_string(),
                categories: search_categories,
                limit: None, // Don't send limit — let indexer use its default (matches Sonarr)
                offset: None,
                external_ids: external_ids.to_vec(),
                search_type,
                season: params.season.clone(),
                episode: params.episode.clone(),
            },
        );

        let result = if let Some(timeout_duration) = params.per_source_timeout {
            match tokio::time::timeout(timeout_duration, search_fut).await {
                Ok(inner) => inner,
                Err(_) => {
                    warn!(source = %source.name, timeout_ms = timeout_duration.as_millis(), "Indexer search timed out");
                    let _ = watch_repo::record_failure(pool, source.id, "search timed out").await;
                    continue;
                }
            }
        } else {
            search_fut.await
        };

        match result {
            Ok(mut source_releases) => {
                let _ = watch_repo::record_success(pool, source.id).await;
                releases.append(&mut source_releases);
            }
            Err(err) if err.status == axum::http::StatusCode::TOO_MANY_REQUESTS => {
                let _ =
                    watch_repo::record_rate_limited(pool, source.id, err.retry_after_seconds).await;
                warn!(source = %source.name, "Indexer rate limited (429)");
            }
            Err(err) => {
                crate::indexers::record_error(pool, source.id, &err).await;
                warn!(error = %err, source = %source.name, "Indexer search failed");
            }
        }
    }

    Ok(releases)
}

/// Callback for quality-aware early exit in cascaded search.
pub type CascadedEarlyExitCheck<'a> = Box<dyn Fn(&[Release]) -> bool + Send + Sync + 'a>;

/// Parameters for Sonarr-style cascaded search (structured → fallback per source).
pub struct CascadedSearchParams<'a> {
    pub ctx: &'a IndexerContext<'a>,
    pub indexers: &'a dyn IndexerResolver,
    pub pool: &'a sqlx::PgPool,
    /// All free-text query patterns for fallback cascade (e.g., "Show S01E05", "Show 1x05").
    pub patterns: &'a [String],
    pub limit: u32,
    pub external_ids: &'a [ExternalId],
    pub media_want: Option<&'a nats_jobs::MediaWant>,
    pub media_type_id: Option<Uuid>,
    /// Season for structured TV search (Newznab `season` param).
    pub season: Option<&'a str>,
    /// Episode for structured TV search (Newznab `ep` param).
    pub episode: Option<&'a str>,
    /// Search context for throttling decisions (backlog vs interactive vs RSS).
    pub search_context: SearchContext,
    /// Optional predicate called after each source's results are collected.
    /// Receives all accumulated releases so far. If it returns `true`, remaining
    /// sources are skipped (quality-aware early exit).
    pub early_exit_check: Option<CascadedEarlyExitCheck<'a>>,
}

/// Search all sources using Sonarr-style structured→fallback per source.
///
/// For each source:
/// 1. If we have external IDs and the source has caps supporting them: try structured search
///    (IDs + season/ep, no free-text query). This is the most efficient — one API call.
/// 2. If structured returned results → done with this source.
/// 3. If structured returned nothing (or wasn't attempted) → fall back to the text cascade,
///    searching with each pattern until results are found.
///
/// Returns all collected releases across all sources, plus a set of source IDs
/// that returned results from structured search (for logging/stats).
pub async fn search_all_sources_cascaded(
    params: &CascadedSearchParams<'_>,
) -> Result<Vec<Release>, String> {
    let ctx = params.ctx;
    let indexers = params.indexers;
    let pool = params.pool;
    let external_ids = params.external_ids;
    let media_want = params.media_want;
    let media_type_id = params.media_type_id;
    let search_type = search_type_from_media_want(media_want);

    let enabled_sources = watch_repo::list_enabled_sources(pool)
        .await
        .map_err(|e| e.to_string())?;

    let searchable_sources: Vec<_> = enabled_sources
        .iter()
        .filter(|s| {
            if let Some(mt_id) = media_type_id {
                s.media_type_ids.is_empty() || s.media_type_ids.contains(&mt_id)
            } else {
                true
            }
        })
        .collect();

    if searchable_sources.is_empty() {
        warn!("No indexer sources configured");
        return Err(
            "No indexer sources configured. Add sources in Settings → Indexer Sources.".to_string(),
        );
    }

    let mut all_releases = Vec::new();

    for source in &searchable_sources {
        if watch_repo::should_skip_for_search(source, &params.search_context) {
            if source.auto_throttle && source.api_limit_daily.is_some_and(|l| l > 0) {
                let limit = source.api_limit_daily.unwrap();
                let pct = (source.api_calls_today as f64 / limit as f64) * 100.0;
                if pct >= 90.0 {
                    info!(
                        source = %source.name,
                        api_calls = source.api_calls_today,
                        api_limit = limit,
                        usage_pct = format!("{pct:.0}%"),
                        "Throttled: skipping source to preserve API budget"
                    );
                }
            } else {
                warn!(source = %source.name, "Skipping source due to rate limit, backoff, or search_mode");
            }
            continue;
        }

        let handler = match indexers.get(&source.handler_type) {
            Some(h) => h,
            None => continue,
        };

        if !handler.capabilities().supports_search {
            continue;
        }

        let search_categories = resolve_categories_for_source(pool, source, media_type_id)
            .await
            .map_err(|e| format!("Category resolution failed: {e}"))?;

        let search_type = resolve_search_type(source, search_type);

        // Try structured search first if we have IDs and the source has caps
        let structured_results = if !external_ids.is_empty()
            && can_do_structured_search(source, search_type, external_ids)
        {
            let filtered_ids = filter_ids_by_caps(source, search_type, external_ids);
            if filtered_ids.is_empty() {
                None
            } else {
                let result = handler
                    .search(
                        ctx,
                        Some(source),
                        IndexerSearchRequest {
                            query: String::new(), // No free-text for structured search
                            categories: search_categories.clone(),
                            limit: None, // Don't send limit — let indexer use its default (matches Sonarr)
                            offset: None,
                            external_ids: filtered_ids,
                            search_type,
                            season: params.season.map(String::from),
                            episode: params.episode.map(String::from),
                        },
                    )
                    .await;

                let _ = watch_repo::increment_api_calls(pool, source.id).await;

                match result {
                    Ok(releases) if !releases.is_empty() => {
                        let _ = watch_repo::record_success(pool, source.id).await;

                        // Sanity check: do any results look relevant to what we searched for?
                        // Some indexers (e.g. DrunkenSlug) return completely unrelated results
                        // for unknown IMDB IDs, which would block the text fallback.
                        let wanted_title = params.media_want.map(|mw| match mw {
                            nats_jobs::MediaWant::Book { title, .. }
                            | nats_jobs::MediaWant::Audiobook { title, .. }
                            | nats_jobs::MediaWant::Movie { title, .. }
                            | nats_jobs::MediaWant::Game { title, .. }
                            | nats_jobs::MediaWant::Comic { title, .. } => title.as_str(),
                            nats_jobs::MediaWant::TvEpisode { show_name, .. }
                            | nats_jobs::MediaWant::TvSeason { show_name, .. } => {
                                show_name.as_str()
                            }
                            nats_jobs::MediaWant::Music { artist, .. } => artist.as_str(),
                            nats_jobs::MediaWant::Software { name, .. } => name.as_str(),
                            nats_jobs::MediaWant::SportEvent { league_name, .. }
                            | nats_jobs::MediaWant::SportSeason { league_name, .. } => {
                                league_name.as_str()
                            }
                        });

                        if let Some(title) = wanted_title {
                            if !any_result_title_relevant(&releases, title) {
                                warn!(
                                    source = %source.name,
                                    count = releases.len(),
                                    wanted = %title,
                                    "Structured search returned results but none match wanted title, falling through to text search"
                                );
                                None
                            } else {
                                info!(
                                    source = %source.name,
                                    count = releases.len(),
                                    "Structured search returned relevant results, skipping fallback cascade"
                                );
                                Some(releases)
                            }
                        } else {
                            info!(
                                source = %source.name,
                                count = releases.len(),
                                "Structured search returned results, skipping fallback cascade"
                            );
                            Some(releases)
                        }
                    }
                    Ok(_) => {
                        debug!(source = %source.name, "Structured search returned no results, trying fallback");
                        let _ = watch_repo::record_success(pool, source.id).await;
                        None
                    }
                    Err(err) if err.status == axum::http::StatusCode::TOO_MANY_REQUESTS => {
                        let _ = watch_repo::record_rate_limited(
                            pool,
                            source.id,
                            err.retry_after_seconds,
                        )
                        .await;
                        warn!(source = %source.name, "Indexer rate limited (429) on structured search");
                        continue; // Skip this source entirely
                    }
                    Err(err) => {
                        crate::indexers::record_error(pool, source.id, &err).await;
                        warn!(error = %err, source = %source.name, "Structured search failed, trying fallback");
                        None
                    }
                }
            }
        } else {
            None
        };

        if let Some(mut releases) = structured_results {
            all_releases.append(&mut releases);
        } else {
            // Fallback: try each pattern in the cascade (free-text, no IDs/season/ep)
            for pattern in params.patterns {
                if pattern.trim().is_empty() {
                    continue;
                }

                let result = handler
                    .search(
                        ctx,
                        Some(source),
                        IndexerSearchRequest {
                            query: pattern.clone(),
                            categories: search_categories.clone(),
                            limit: None, // Don't send limit — let indexer use its default (matches Sonarr)
                            offset: None,
                            external_ids: Vec::new(), // No IDs for fallback
                            search_type,
                            season: None,
                            episode: None,
                        },
                    )
                    .await;

                let _ = watch_repo::increment_api_calls(pool, source.id).await;

                match result {
                    Ok(mut releases) => {
                        let _ = watch_repo::record_success(pool, source.id).await;
                        let count = releases.len();
                        all_releases.append(&mut releases);
                        if count > 0 {
                            debug!(
                                source = %source.name,
                                pattern = %pattern,
                                count,
                                "Fallback pattern returned results, stopping cascade for this source"
                            );
                            break; // Got results from this source, move to next source
                        }
                    }
                    Err(err) if err.status == axum::http::StatusCode::TOO_MANY_REQUESTS => {
                        let _ = watch_repo::record_rate_limited(
                            pool,
                            source.id,
                            err.retry_after_seconds,
                        )
                        .await;
                        warn!(source = %source.name, "Indexer rate limited (429) on fallback");
                        break; // Rate limited — stop trying patterns on this source
                    }
                    Err(err) => {
                        crate::indexers::record_error(pool, source.id, &err).await;
                        warn!(error = %err, source = %source.name, pattern = %pattern, "Fallback search failed");
                        break; // Error — stop trying patterns on this source
                    }
                }
            }
        }

        // Quality-aware early exit: if accumulated results already contain a
        // satisfactory match, skip remaining sources.
        if let Some(ref check) = params.early_exit_check
            && check(&all_releases)
        {
            info!(
                source = %source.name,
                total_releases = all_releases.len(),
                "Early exit: satisfactory results found, skipping remaining sources"
            );
            break;
        }
    }

    Ok(all_releases)
}

/// Check if a source can do structured search for the given search type with the available IDs.
pub(crate) fn can_do_structured_search(
    source: &watch_repo::IndexerSourceRow,
    search_type: SearchType,
    external_ids: &[ExternalId],
) -> bool {
    // Only Newznab/Torznab handlers support structured search
    if source.handler_type != "newznab" && source.handler_type != "torznab" {
        return false;
    }

    // Only TV and Movie search types benefit from structured search
    if !matches!(search_type, SearchType::TvSearch | SearchType::MovieSearch) {
        return false;
    }

    let Some(caps) = parse_source_caps(source) else {
        // No caps stored — safe default: send IDs anyway (indexers ignore unknown params)
        return !external_ids.is_empty();
    };

    if !caps.search_available(search_type) {
        return false;
    }

    // Check if at least one of our IDs is supported by this source
    external_ids.iter().any(|id| {
        let param_name = id_source_to_param(&id.source);
        param_name.is_some_and(|p| caps.supports_param(search_type, p))
    })
}

/// Filter external IDs to only those supported by the source's caps.
pub(crate) fn filter_ids_by_caps(
    source: &watch_repo::IndexerSourceRow,
    search_type: SearchType,
    external_ids: &[ExternalId],
) -> Vec<ExternalId> {
    let Some(caps) = parse_source_caps(source) else {
        // No caps — send all IDs (safe default)
        return external_ids.to_vec();
    };

    external_ids
        .iter()
        .filter(|id| {
            id_source_to_param(&id.source).is_some_and(|p| caps.supports_param(search_type, p))
        })
        .cloned()
        .collect()
}

fn parse_source_caps(source: &watch_repo::IndexerSourceRow) -> Option<NewznabCaps> {
    source
        .capabilities
        .as_ref()
        .and_then(|v| serde_json::from_value::<NewznabCaps>(v.clone()).ok())
}

fn id_source_to_param(source: &ExternalIdSource) -> Option<&'static str> {
    match source {
        ExternalIdSource::Imdb => Some("imdbid"),
        ExternalIdSource::Tmdb => Some("tmdbid"),
        ExternalIdSource::Tvdb => Some("tvdbid"),
        ExternalIdSource::TvMaze => Some("tvmazeid"),
        _ => None,
    }
}

/// Check if any structured search result's title shares significant words with
/// the wanted title. Used to detect when an indexer returns completely unrelated
/// results for a structured (IMDB/TMDB) query — e.g. DrunkenSlug returning
/// random movies for an unknown IMDB ID.
///
/// Returns `true` if at least one result looks plausibly relevant, `false` if
/// none do (caller should fall through to text search).
pub(crate) fn any_result_title_relevant(releases: &[Release], wanted_title: &str) -> bool {
    let wanted_words = crate::services::title_tokens::title_keywords(wanted_title);
    if wanted_words.is_empty() {
        return true; // Can't check, assume relevant
    }

    releases.iter().any(|r| {
        let release_words = crate::services::title_tokens::title_keywords(&r.title);
        // At least one significant word from the wanted title appears in this release
        wanted_words
            .iter()
            .any(|w| release_words.iter().any(|rw| rw == w))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use time::OffsetDateTime;

    fn test_source(
        handler_type: &str,
        caps: Option<serde_json::Value>,
    ) -> watch_repo::IndexerSourceRow {
        watch_repo::IndexerSourceRow {
            id: Uuid::nil(),
            name: "test".to_string(),
            handler_type: handler_type.to_string(),
            handler_config: json!({}),
            category_ids: None,
            media_type_ids: vec![],
            enabled: true,
            last_check_at: None,
            last_error: None,
            state: None,
            created_at: OffsetDateTime::now_utc(),
            priority: 0,
            last_success_at: None,
            last_failure_at: None,
            consecutive_failures: 0,
            seed_ratio: None,
            seed_time_minutes: None,
            rate_limited_until: None,
            auth_failed_at: None,
            capabilities: caps,
            caps_fetched_at: None,
            api_limit_daily: None,
            api_calls_today: 0,
            last_api_reset_at: OffsetDateTime::now_utc(),
            search_mode: "full".to_string(),
            auto_throttle: true,
            debrid_mode: "never".to_string(),
        }
    }

    fn imdb_id(id: &str) -> ExternalId {
        ExternalId {
            source: ExternalIdSource::Imdb,
            id: id.to_string(),
        }
    }

    fn tvdb_id(id: &str) -> ExternalId {
        ExternalId {
            source: ExternalIdSource::Tvdb,
            id: id.to_string(),
        }
    }

    fn tmdb_id(id: &str) -> ExternalId {
        ExternalId {
            source: ExternalIdSource::Tmdb,
            id: id.to_string(),
        }
    }

    fn caps_with_tv_and_movie() -> serde_json::Value {
        json!({
            "search": { "available": true, "params": ["q"] },
            "tv_search": { "available": true, "params": ["q", "tvdbid", "imdbid", "season", "ep"] },
            "movie_search": { "available": true, "params": ["q", "imdbid"] },
            "book_search": { "available": false, "params": [] },
            "audio_search": { "available": false, "params": [] }
        })
    }

    #[test]
    fn test_can_do_structured_search_newznab_with_caps() {
        let source = test_source("newznab", Some(caps_with_tv_and_movie()));
        let ids = vec![tvdb_id("12345")];
        assert!(can_do_structured_search(
            &source,
            SearchType::TvSearch,
            &ids
        ));
    }

    #[test]
    fn test_can_do_structured_search_rejects_non_newznab() {
        let source = test_source("torrentday", Some(caps_with_tv_and_movie()));
        let ids = vec![tvdb_id("12345")];
        assert!(!can_do_structured_search(
            &source,
            SearchType::TvSearch,
            &ids
        ));
    }

    #[test]
    fn test_can_do_structured_search_rejects_book_search() {
        let source = test_source("newznab", Some(caps_with_tv_and_movie()));
        let ids = vec![imdb_id("tt123")];
        assert!(!can_do_structured_search(
            &source,
            SearchType::BookSearch,
            &ids
        ));
    }

    #[test]
    fn test_can_do_structured_search_no_caps_still_allows() {
        // Safe default: send IDs when caps haven't been fetched
        let source = test_source("newznab", None);
        let ids = vec![imdb_id("tt123")];
        assert!(can_do_structured_search(
            &source,
            SearchType::MovieSearch,
            &ids
        ));
    }

    #[test]
    fn test_can_do_structured_search_rejects_unsupported_id() {
        // Source only supports imdbid for movies, not tmdbid
        let source = test_source("newznab", Some(caps_with_tv_and_movie()));
        let ids = vec![tmdb_id("999")];
        assert!(!can_do_structured_search(
            &source,
            SearchType::MovieSearch,
            &ids
        ));
    }

    #[test]
    fn test_can_do_structured_search_at_least_one_supported() {
        let source = test_source("newznab", Some(caps_with_tv_and_movie()));
        // tmdbid not supported for movie, but imdbid is
        let ids = vec![tmdb_id("999"), imdb_id("tt123")];
        assert!(can_do_structured_search(
            &source,
            SearchType::MovieSearch,
            &ids
        ));
    }

    #[test]
    fn test_filter_ids_by_caps_filters_unsupported() {
        let source = test_source("newznab", Some(caps_with_tv_and_movie()));
        let ids = vec![tmdb_id("999"), imdb_id("tt123"), tvdb_id("456")];
        let filtered = filter_ids_by_caps(&source, SearchType::MovieSearch, &ids);
        assert_eq!(filtered.len(), 1);
        assert_eq!(filtered[0].id, "tt123");
    }

    #[test]
    fn test_filter_ids_by_caps_returns_all_without_caps() {
        let source = test_source("newznab", None);
        let ids = vec![tmdb_id("999"), imdb_id("tt123")];
        let filtered = filter_ids_by_caps(&source, SearchType::TvSearch, &ids);
        assert_eq!(filtered.len(), 2);
    }

    #[test]
    fn test_filter_ids_by_caps_tv_search() {
        let source = test_source("newznab", Some(caps_with_tv_and_movie()));
        // TV search supports tvdbid and imdbid but not tmdbid
        let ids = vec![tmdb_id("999"), imdb_id("tt123"), tvdb_id("456")];
        let filtered = filter_ids_by_caps(&source, SearchType::TvSearch, &ids);
        assert_eq!(filtered.len(), 2);
        let filtered_ids: Vec<&str> = filtered.iter().map(|id| id.id.as_str()).collect();
        assert!(filtered_ids.contains(&"tt123"));
        assert!(filtered_ids.contains(&"456"));
    }

    #[test]
    fn test_parse_source_caps_valid() {
        let source = test_source("newznab", Some(caps_with_tv_and_movie()));
        let caps = parse_source_caps(&source).unwrap();
        assert!(caps.tv_search.available);
        assert!(caps.movie_search.available);
        assert!(!caps.book_search.available);
        assert!(caps.tv_search.params.contains(&"tvdbid".to_string()));
    }

    #[test]
    fn test_parse_source_caps_none() {
        let source = test_source("newznab", None);
        assert!(parse_source_caps(&source).is_none());
    }

    fn test_release(title: &str) -> Release {
        Release {
            guid: "test".to_string(),
            title: title.to_string(),
            file_name: None,
            download_url: None,
            magnet_url: None,
            info_url: None,
            size: 0,
            publish_date: String::new(),
            category_ids: vec![2000],
            indexer: "test".to_string(),
            protocol: "usenet".to_string(),
            info_hash: None,
            files: None,
            seeders: None,
            peers: None,
            leechers: None,
            grabs: None,
            votes: None,
            indexer_id: None,
            indexer_source_id: None,
            raw_data: serde_json::Value::Null,
            external_ids: vec![],
            media: None,
        }
    }

    #[test]
    fn test_any_result_title_relevant_matching() {
        let releases = vec![
            test_release("The.Super.Mario.Galaxy.Movie.2026.1080p.WEB-DL"),
            test_release("Some.Other.Movie.2025.720p"),
        ];
        assert!(any_result_title_relevant(
            &releases,
            "The Super Mario Galaxy Movie"
        ));
    }

    #[test]
    fn test_any_result_title_relevant_no_match() {
        // DrunkenSlug scenario: completely unrelated results
        let releases = vec![
            test_release("Louis.Theroux.Inside.the.Manosphere.2026.2160p.NF.WEB.DL"),
            test_release("Pesugihan.Sate.Gagak.2025.1080p.NF.WEB.DL"),
            test_release("Brides (2025) 1080p WEBRip-LAMA"),
            test_release("My Fathers Vietnam (2015) 1080p BluRay-LAMA"),
        ];
        assert!(!any_result_title_relevant(
            &releases,
            "The Super Mario Galaxy Movie"
        ));
    }

    #[test]
    fn test_any_result_title_relevant_partial_match() {
        // One word overlap should be enough (e.g. sequel/remake with shared word)
        let releases = vec![test_release("Galaxy.Quest.1999.BluRay.1080p")];
        assert!(any_result_title_relevant(
            &releases,
            "The Super Mario Galaxy Movie"
        ));
    }

    #[test]
    fn test_any_result_title_relevant_empty_releases() {
        assert!(!any_result_title_relevant(&[], "Some Movie"));
    }

    #[test]
    fn test_any_result_title_relevant_empty_title() {
        let releases = vec![test_release("Something")];
        assert!(any_result_title_relevant(&releases, ""));
    }

    #[test]
    fn test_any_result_title_relevant_stopwords_only() {
        // Title is only stopwords — can't meaningfully check, return true
        let releases = vec![test_release("Something")];
        assert!(any_result_title_relevant(&releases, "The A"));
    }

    #[test]
    fn test_any_result_title_relevant_case_insensitive() {
        let releases = vec![test_release("scrubs.s01e01.720p")];
        assert!(any_result_title_relevant(&releases, "Scrubs"));
    }

    #[test]
    fn test_id_source_to_param_mappings() {
        assert_eq!(id_source_to_param(&ExternalIdSource::Imdb), Some("imdbid"));
        assert_eq!(id_source_to_param(&ExternalIdSource::Tmdb), Some("tmdbid"));
        assert_eq!(id_source_to_param(&ExternalIdSource::Tvdb), Some("tvdbid"));
        assert_eq!(
            id_source_to_param(&ExternalIdSource::TvMaze),
            Some("tvmazeid")
        );
    }
}
