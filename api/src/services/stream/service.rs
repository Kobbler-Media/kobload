//! Stream search service — searches indexers, scores releases, checks debrid availability.

use std::sync::LazyLock;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use sqlx::PgPool;
use tracing::{info, warn};
use uuid::Uuid;

use super::availability::known_availability;
use super::ranking::{
    KnownAvailability, RankCandidate, RankPreferences, RankProvider, rank_options,
};
use crate::debrid::{DebridProviderResolver, DebridProviderType};
use crate::decision_log::types::{LogProviderCall, LogRelease};
use crate::decision_log::{self, DecisionCollector};
use crate::hash_sources::{EztvClient, TorrentioClient, ZileanClient};
use crate::indexers::cardigann::DefinitionCache;
use crate::indexers::{IndexerContext, IndexerResolver};
use crate::parsing::parse_release;
use crate::repositories::app_settings;
use crate::repositories::hash_index::{self, HashFileEntry, HashIndexEntry, HashIndexRow};
use crate::repositories::quality_profiles;
use crate::repositories::watch as watch_repo;
use crate::scene_mappings::SceneMappingCache;
use crate::scene_titles::TvNaming;
use crate::services::search::{SearchIndexersParams, search_indexers};
use crate::services::want::scoring::SearchContext;
use crate::trash_filters::TrashFilterCache;
use crate::watch::Release;
use crate::xem::XemCache;
use nats_jobs::types::{StreamOption, StreamSearchRequest, StreamSearchResponse, StreamSourceType};

/// Default maximum results returned.
const DEFAULT_LIMIT: u32 = 50;

/// How long Tier 1 waits for Torrentio before proceeding without it.
///
/// Torrentio is our only bulk source of debrid cache markers. Warm lookups
/// return in ~30ms, but a cold one (a title nobody has requested recently) was
/// measured at 400-465ms in debrid mode, so a 500ms budget lost the markers
/// precisely when they mattered most.
const TORRENTIO_GRACE: Duration = Duration::from_millis(1200);

/// TTL for the indexer result cache.
const INDEXER_CACHE_TTL: Duration = Duration::from_secs(900); // 15 minutes

/// In-memory cache for indexer search results keyed by query + media_want.
/// Value is (scored results, original releases, insertion time).
type IndexerCacheEntry = (Vec<crate::watch::ProcessedRelease>, Vec<Release>, Instant);

static INDEXER_RESULT_CACHE: LazyLock<DashMap<String, IndexerCacheEntry>> =
    LazyLock::new(DashMap::new);

/// Build a cache key from the search request's query and media_want.
fn indexer_cache_key(request: &StreamSearchRequest) -> String {
    // Use serde_json serialization for a stable, deterministic key.
    // media_want encodes title, year, identifiers — everything that affects search.
    let mw_part = request
        .media_want
        .as_ref()
        .and_then(|mw| serde_json::to_string(mw).ok())
        .unwrap_or_default();
    format!("{}|{}", request.query, mw_part)
}

/// Context dependencies for stream search.
pub struct StreamSearchContext<'a> {
    pub pool: &'a PgPool,
    pub http_client: &'a reqwest::Client,
    pub indexers: &'a dyn IndexerResolver,
    pub debrid_providers: &'a dyn DebridProviderResolver,
    pub definitions: &'a DefinitionCache,
    pub xem_cache: &'a XemCache,
    pub scene_mappings: &'a SceneMappingCache,
    pub trash_filters: &'a TrashFilterCache,
    /// Broadcast channel for SSE events (used to emit warm cache ready events).
    pub download_events: &'a tokio::sync::broadcast::Sender<crate::types::DownloadStreamEvent>,
    /// Arc-wrapped debrid providers for spawning background warming tasks.
    pub debrid_providers_arc: std::sync::Arc<dyn DebridProviderResolver>,
}

/// Errors from the stream search pipeline.
#[derive(Debug)]
pub enum StreamSearchError {
    /// No indexer sources configured or all filtered out.
    NoSources(String),
    /// No debrid providers configured or enabled.
    NoDebridProviders,
    /// Database error.
    Database(String),
    /// Indexer search failed across all sources.
    SearchFailed(String),
    /// Debrid provider error.
    DebridError(String),
}

impl std::fmt::Display for StreamSearchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoSources(msg) => write!(f, "No indexer sources: {msg}"),
            Self::NoDebridProviders => write!(f, "No debrid providers configured"),
            Self::Database(msg) => write!(f, "Database error: {msg}"),
            Self::SearchFailed(msg) => write!(f, "Search failed: {msg}"),
            Self::DebridError(msg) => write!(f, "Debrid error: {msg}"),
        }
    }
}

impl std::error::Error for StreamSearchError {}

/// Row from debrid_providers table.
struct DebridProviderRow {
    id: Uuid,
    provider_type: String,
    api_key: String,
}

/// Per-phase wall-clock timings for a single `stream_search` call.
/// Populated as the request progresses and dumped in a single log line at the
/// end so we can see where time is spent without scraping multiple entries.
#[derive(Debug, Default)]
struct StreamSearchTimings {
    hash_index_ms: u64,
    torrentio_wait_ms: u64,
    tier1_score_ms: u64,
    tier2_ms: u64,
    tier2_ran: bool,
    cardigann_ms: u64,
    cardigann_ran: bool,
    dead_hashes_ms: u64,
    dead_hashes_supplementary_count: usize,
    options_build_ms: u64,
    total_ms: u64,
}

impl StreamSearchTimings {
    fn log_summary(&self, option_count: usize, message: &'static str) {
        info!(
            option_count,
            total_ms = self.total_ms,
            hash_index_ms = self.hash_index_ms,
            torrentio_wait_ms = self.torrentio_wait_ms,
            tier1_score_ms = self.tier1_score_ms,
            tier2_ran = self.tier2_ran,
            tier2_ms = self.tier2_ms,
            cardigann_ran = self.cardigann_ran,
            cardigann_ms = self.cardigann_ms,
            dead_hashes_ms = self.dead_hashes_ms,
            dead_hashes_supplementary = self.dead_hashes_supplementary_count,
            options_build_ms = self.options_build_ms,
            message
        );
    }
}

/// Search for streamable content via debrid providers.
///
/// Pipeline: search indexers -> score releases -> filter to torrents with hashes ->
/// check debrid availability (cache then API) -> build ranked StreamOption results.
pub async fn stream_search(
    ctx: &StreamSearchContext<'_>,
    request: &StreamSearchRequest,
) -> Result<StreamSearchResponse, StreamSearchError> {
    let mut request = request.clone();
    if let Some(want) = request.media_want.as_mut() {
        crate::learned_aliases::apply(ctx.pool, want).await;
    }
    let request = &request;
    let limit = request.limit.unwrap_or(DEFAULT_LIMIT);

    // Log incoming request details for debugging
    let (imdb_id, tmdb_id, title, extra) = extract_request_ids(request);
    info!(
        media_type = ?request.media_type,
        %imdb_id,
        %tmdb_id,
        %title,
        %extra,
        limit,
        "Stream search request"
    );

    let errors: Vec<String> = Vec::new();

    // Decision log: create collector if debrid logging is enabled
    let debrid_logging = app_settings::get_debrid_logging_enabled(ctx.pool)
        .await
        .unwrap_or(false);
    let collector = if debrid_logging {
        let c = DecisionCollector::new("debrid_search", None::<Uuid>, None::<Uuid>, false);
        if !title.is_empty() {
            c.record_query(&title);
        }
        if !imdb_id.is_empty() {
            c.record_query(format!("IMDB: {imdb_id}"));
        }
        Some(c)
    } else {
        None
    };

    // 1. Load debrid providers — early return with user-facing error if none configured
    let providers = load_enabled_debrid_providers(ctx.pool).await?;
    if providers.is_empty() {
        return Ok(StreamSearchResponse {
            options: Vec::new(),
            errors: vec![
                "No debrid providers configured. Add providers in Settings \u{2192} Debrid Providers.".to_string(),
            ],
        });
    }

    // 2. Load debrid-eligible indexer sources
    let debrid_sources = watch_repo::list_debrid_sources(ctx.pool)
        .await
        .map_err(|e| StreamSearchError::Database(e.to_string()))?;

    if debrid_sources.is_empty() {
        return Ok(StreamSearchResponse {
            options: Vec::new(),
            errors: vec![
                "No indexers enabled for debrid streaming. Set debrid_mode to 'Always' or 'Both' on at least one indexer in Settings \u{2192} Indexer Sources.".to_string(),
            ],
        });
    }

    // A resolve has to list the account before adding (RD does not dedup), and the
    // listing takes about a second; starting it now hides it behind the search.
    if let Some(provider_type) = DebridProviderType::parse(&providers[0].provider_type) {
        super::account_index::prefetch(
            providers[0].id,
            ctx.debrid_providers.get(&provider_type),
            ctx.http_client.clone(),
            providers[0].api_key.clone(),
        );
    }

    // Check if any provider supports usenet (TorBox)
    let has_usenet_provider = providers.iter().any(|p| p.provider_type == "torbox");

    // 3. TIERED SOURCES: Hash index (instant) + external APIs (tiered deadlines)
    //
    // Hash index returns instantly. Torrentio gets a 500ms grace period.
    // If hash_index + torrentio produce good-enough results, return immediately.
    // Otherwise, wait up to 3s for secondary sources (StremThru, EZTV).
    // All external tasks run to completion in the background, indexing results
    // into the hash index regardless of whether the caller used them.

    let search_start = Instant::now();
    let mut timings = StreamSearchTimings::default();

    // Fire hash index + all external APIs concurrently
    let hash_index_start = Instant::now();
    let hash_index_releases = search_hash_index(ctx.pool, request).await;
    timings.hash_index_ms = hash_index_start.elapsed().as_millis() as u64;
    // Torrentio annotates each stream with the debrid cache status for the key
    // it is given, so hand it our Real-Debrid key when we have one. This is the
    // only remaining bulk "is it cached" signal — RD's own instantAvailability
    // endpoint is gone (error_code 37).
    let real_debrid_key = providers
        .iter()
        .find(|p| p.provider_type == "real_debrid")
        .map(|p| p.api_key.clone());
    let api_handles = spawn_external_apis(ctx.http_client, ctx.pool, request, real_debrid_key);

    if let Some(ref log) = collector {
        log.record_provider_call(LogProviderCall {
            id: Uuid::new_v4(),
            indexer_source_id: None,
            source_name: "Hash Index".to_string(),
            handler_type: "hash_index".to_string(),
            search_type: Some(if imdb_id.is_empty() { "title" } else { "imdb" }.to_string()),
            query: Some(if imdb_id.is_empty() {
                title.clone()
            } else {
                imdb_id.clone()
            }),
            categories: None,
            external_ids: None,
            results_count: hash_index_releases.len() as i32,
            results_after_dedup: hash_index_releases.len() as i32,
            status: "success".to_string(),
            error_message: None,
            duration_ms: None,
        });
    }

    // Prepare scoring dependencies early (these are cheap/cached lookups)
    let naming = match request.media_want {
        Some(ref mw) => {
            TvNaming::resolve(
                ctx.scene_mappings,
                ctx.xem_cache,
                ctx.http_client,
                ctx.pool,
                mw,
            )
            .await
        }
        None => TvNaming::default(),
    };
    let patterns = build_patterns(request, &naming);
    let quality_profile = load_quality_profile(ctx.pool, request).await;
    let trash_filter_set = ctx.trash_filters.get_filters();

    // Helper closure: score a set of releases through the unified pipeline.
    // Returns the FULL set (candidates + rejects) so the decision log can
    // reuse the scoring results instead of re-scoring rejects.
    let score_releases = |releases: &[Release]| -> Vec<crate::watch::ProcessedRelease> {
        if releases.is_empty() {
            return Vec::new();
        }
        let config = scoring_config(
            request,
            &patterns,
            quality_profile.as_ref(),
            trash_filter_set.as_deref(),
            &naming,
        );
        crate::services::want::scoring::score_batch(releases, &config)
    };

    // Destructure handles for tiered consumption
    let ExternalApiHandles {
        torrentio: torrentio_rx,
        zilean: zilean_rx,
        eztv: eztv_rx,
        torrentio_query: api_torrentio_query,
        zilean_query: api_zilean_query,
        eztv_query: api_eztv_query,
    } = api_handles;

    // Tier 1: Hash index (already have) + Torrentio (500ms grace)
    let mut fast_releases = hash_index_releases;
    let mut torrentio_entries: Vec<HashIndexEntry> = Vec::new();
    let mut torrentio_files: Vec<HashFileEntry> = Vec::new();
    // Torrentio's per-hash view of what is cached on the configured debrid
    // account. Only populated when an API key was attached to the request;
    // absent hashes mean "unknown", never "uncached".
    let mut torrentio_cached: std::collections::HashMap<String, bool> =
        std::collections::HashMap::new();
    let mut binge_groups: std::collections::HashMap<String, String> =
        std::collections::HashMap::new();
    let mut torrentio_used = false;
    let mut zilean_used = false;
    let mut eztv_used = false;
    let mut zilean_count = 0usize;
    let mut eztv_count = 0usize;

    // Torrentio is the only source of debrid cache markers, and in debrid mode
    // a cold lookup measures 400-465ms against the 500ms budget — so the old
    // deadline dropped exactly the markers that make ranking useful, on exactly
    // the titles nobody had searched yet. Widen it: the cost of waiting is a
    // few hundred ms once, versus the user walking blocked options for 40s.
    let torrentio_wait_start = Instant::now();
    if let Ok(Ok(results)) = tokio::time::timeout(TORRENTIO_GRACE, torrentio_rx).await
        && !results.is_empty()
    {
        info!(
            count = results.len(),
            grace_ms = TORRENTIO_GRACE.as_millis() as u64,
            "Torrentio responded within grace period"
        );
        for tr in results {
            if let Some(cached) = tr.debrid_cached {
                torrentio_cached.insert(tr.entry.info_hash.to_lowercase(), cached);
            }
            if let Some(group) = tr.binge_group {
                binge_groups.insert(tr.entry.info_hash.to_lowercase(), group);
            }
            torrentio_entries.push(tr.entry);
            if let Some(file) = tr.file {
                torrentio_files.push(file);
            }
        }
        let torrentio_releases = external_entries_to_releases(&torrentio_entries, &torrentio_files);
        fast_releases.extend(torrentio_releases);
        torrentio_used = true;
    }
    timings.torrentio_wait_ms = torrentio_wait_start.elapsed().as_millis() as u64;

    // Dedup before scoring — same torrent may appear in hash_index and torrentio
    dedup_releases(&mut fast_releases);

    // Kick off the availability lookup NOW, concurrently with scoring / Tier 2
    // wait / Cardigann fallback. We use hashes known at this point; any later
    // additions (Cardigann) get a supplementary synchronous query below.
    let default_provider_id = providers[0].id;
    let initial_dead_hash_inputs = collect_torrent_hashes(&fast_releases);
    let dead_hashes_handle = {
        let pool = ctx.pool.clone();
        let hashes = initial_dead_hash_inputs.clone();
        tokio::spawn(async move { known_availability(&pool, default_provider_id, &hashes).await })
    };

    // Check if Tier 1 is good enough.
    // `scored` holds the FULL scored set (candidates + rejects); we track
    // which guids have already been scored so Tier 2 arrivals only score
    // the delta instead of re-scoring the whole accumulated set.
    let tier1_score_start = Instant::now();
    let mut scored = score_releases(&fast_releases);
    let mut scored_guids: std::collections::HashSet<String> =
        scored.iter().map(|pr| pr.release.guid.clone()).collect();
    timings.tier1_score_ms = tier1_score_start.elapsed().as_millis() as u64;
    let has_candidate = |scored: &[crate::watch::ProcessedRelease]| -> bool {
        scored.iter().any(|pr| pr.is_candidate())
    };
    let mut tier = if has_candidate(&scored) {
        if torrentio_used {
            "hash_index+torrentio"
        } else {
            "hash_index"
        }
    } else {
        ""
    };

    // Incremental scoring helper: dedup `fast_releases`, then score only
    // releases whose guid isn't already in `scored_guids` and extend `scored`.
    let score_new = |fast_releases: &mut Vec<Release>,
                     scored: &mut Vec<crate::watch::ProcessedRelease>,
                     scored_guids: &mut std::collections::HashSet<String>| {
        dedup_releases(fast_releases);
        let new_releases: Vec<Release> = fast_releases
            .iter()
            .filter(|r| !scored_guids.contains(&r.guid))
            .cloned()
            .collect();
        if new_releases.is_empty() {
            return;
        }
        let new_scored = score_releases(&new_releases);
        for pr in &new_scored {
            scored_guids.insert(pr.release.guid.clone());
        }
        scored.extend(new_scored);
    };

    // Tier 2: If no candidates yet, wait up to 3s for secondary sources
    let tier2_start = Instant::now();
    if !has_candidate(&scored) {
        timings.tier2_ran = true;
        info!(
            elapsed_ms = search_start.elapsed().as_millis(),
            "Tier 1 insufficient — waiting up to 3s for secondary sources"
        );

        let secondary_deadline = Duration::from_secs(3);
        let deadline = tokio::time::sleep(secondary_deadline);
        tokio::pin!(deadline);

        // Try to collect both zilean and eztv before deadline
        let mut zilean_done = false;
        let mut eztv_done = false;
        let mut zilean_rx = Some(zilean_rx);
        let mut eztv_rx = Some(eztv_rx);

        loop {
            tokio::select! {
                result = async { zilean_rx.as_mut().unwrap().await }, if zilean_rx.is_some() && !zilean_done => {
                    zilean_done = true;
                    if let Ok(entries) = result {
                        zilean_count = entries.len();
                        if !entries.is_empty() {
                            info!(count = entries.len(), "Zilean/StremThru responded");
                            let releases = external_entries_to_releases(&entries, &[]);
                            fast_releases.extend(releases);
                            zilean_used = true;
                        }
                    }
                    zilean_rx = None;
                    // Score only the new releases added by this arrival
                    score_new(&mut fast_releases, &mut scored, &mut scored_guids);
                    if has_candidate(&scored) {
                        tier = "hash_index+all_apis";
                        break;
                    }
                }
                result = async { eztv_rx.as_mut().unwrap().await }, if eztv_rx.is_some() && !eztv_done => {
                    eztv_done = true;
                    if let Ok(entries) = result {
                        eztv_count = entries.len();
                        if !entries.is_empty() {
                            info!(count = entries.len(), "EZTV responded");
                            let releases = external_entries_to_releases(&entries, &[]);
                            fast_releases.extend(releases);
                            eztv_used = true;
                        }
                    }
                    eztv_rx = None;
                    // Score only the new releases added by this arrival
                    score_new(&mut fast_releases, &mut scored, &mut scored_guids);
                    if has_candidate(&scored) {
                        tier = "hash_index+all_apis";
                        break;
                    }
                }
                () = &mut deadline => {
                    info!("Secondary source deadline (3s) reached");
                    // Score anything that arrived just before the deadline
                    score_new(&mut fast_releases, &mut scored, &mut scored_guids);
                    tier = "hash_index+all_apis";
                    break;
                }
            }

            // All sources responded
            if zilean_done && eztv_done {
                score_new(&mut fast_releases, &mut scored, &mut scored_guids);
                tier = "hash_index+all_apis";
                break;
            }
        }
    }
    if timings.tier2_ran {
        timings.tier2_ms = tier2_start.elapsed().as_millis() as u64;
    }

    // Build stats for decision logging
    let api_stats = ExternalApiStats {
        zilean_count,
        zilean_query: api_zilean_query,
        zilean_used,
        torrentio_count: torrentio_entries.len(),
        torrentio_query: api_torrentio_query,
        torrentio_used,
        eztv_count,
        eztv_query: api_eztv_query,
        eztv_used,
    };

    if let Some(ref log) = collector {
        for (name, count, query, used) in [
            (
                "StremThru (Zilean)",
                api_stats.zilean_count,
                &api_stats.zilean_query,
                api_stats.zilean_used,
            ),
            (
                "Torrentio",
                api_stats.torrentio_count,
                &api_stats.torrentio_query,
                api_stats.torrentio_used,
            ),
            (
                "EZTV",
                api_stats.eztv_count,
                &api_stats.eztv_query,
                api_stats.eztv_used,
            ),
        ] {
            log.record_provider_call(LogProviderCall {
                id: Uuid::new_v4(),
                indexer_source_id: None,
                source_name: name.to_string(),
                handler_type: "external_api".to_string(),
                search_type: None,
                query: Some(query.clone()),
                categories: None,
                external_ids: None,
                results_count: count as i32,
                results_after_dedup: count as i32,
                status: if used { "used" } else { "background" }.to_string(),
                error_message: None,
                duration_ms: None,
            });
        }
    }

    if !fast_releases.is_empty() {
        info!(
            count = fast_releases.len(),
            scored = scored.iter().filter(|pr| pr.is_candidate()).count(),
            tier,
            elapsed_ms = search_start.elapsed().as_millis(),
            "Fast source results"
        );
    }

    let mut releases = fast_releases;

    // 4. If fast sources returned no candidates, fall back to Cardigann live search
    let cardigann_start = Instant::now();
    if !has_candidate(&scored) {
        timings.cardigann_ran = true;
        info!("No fast source results — falling back to Cardigann live search");
        let cache_key = indexer_cache_key(request);
        let cached_result = INDEXER_RESULT_CACHE.get(&cache_key).and_then(|entry| {
            let (ref s, ref r, inserted_at) = *entry;
            if inserted_at.elapsed() < INDEXER_CACHE_TTL {
                Some((s.clone(), r.clone()))
            } else {
                None
            }
        });
        if cached_result.is_none() {
            INDEXER_RESULT_CACHE.remove(&cache_key);
        }
        let (cg_scored, cg_releases) = if let Some(cached) = cached_result {
            info!("Indexer result cache hit");
            cached
        } else {
            match search_and_score_indexers(ctx, request, &debrid_sources, &cache_key, &naming)
                .await
            {
                Ok(result) => result,
                Err(e) => {
                    warn!(error = %e, "Cardigann fallback search failed");
                    (Vec::new(), Vec::new())
                }
            }
        };
        if let Some(ref log) = collector {
            log.record_provider_call(LogProviderCall {
                id: Uuid::new_v4(),
                indexer_source_id: None,
                source_name: "Cardigann".to_string(),
                handler_type: "cardigann".to_string(),
                search_type: Some("fallback".to_string()),
                query: Some(build_search_query(request)),
                categories: None,
                external_ids: None,
                results_count: cg_scored.len() as i32,
                results_after_dedup: cg_scored.len() as i32,
                status: "success".to_string(),
                error_message: None,
                duration_ms: None,
            });
        }
        scored = cg_scored;
        releases.extend(cg_releases);
    }
    if timings.cardigann_ran {
        timings.cardigann_ms = cardigann_start.elapsed().as_millis() as u64;
    }

    // Record ALL evaluated releases in the decision log for debugging.
    // `scored` now contains both candidates and rejects from a single
    // scoring pass, so we can log each with its actual decision directly.
    if let Some(ref log) = collector {
        for pr in &scored {
            let is_candidate = pr.is_candidate();
            log.record_release(LogRelease {
                provider_call_id: None,
                release_title: pr.release.title.clone(),
                release_guid: Some(pr.release.guid.clone()),
                release_size: Some(pr.release.size),
                protocol: Some(pr.release.protocol.clone()),
                indexer_name: Some(pr.release.indexer.clone()),
                match_score: pr.match_score,
                matched_pattern: pr.matched_pattern.clone(),
                match_mode: Some("any".to_string()),
                quality_score: pr.quality_score,
                quality_score_breakdown: None,
                total_score: Some(pr.total_score),
                parsed_quality: pr
                    .parsed_quality
                    .as_ref()
                    .and_then(|pq| serde_json::to_value(pq).ok()),
                structural_result: None,
                match_explanation: pr.match_explanation.clone(),
                decision: if is_candidate {
                    "candidate".to_string()
                } else {
                    pr.decision.to_string()
                },
                decision_reason: if is_candidate {
                    None
                } else {
                    pr.reject_reasons.first().map(|r| r.to_string())
                },
                episode_key: None,
                is_best: false,
                release_data: None,
            });
        }
    }

    if !has_candidate(&scored) {
        if let Some(c) = collector {
            c.set_outcome("no_results");
            decision_log::spawn_flush(ctx.pool.clone(), c.into_flush_payload());
        }
        timings.total_ms = search_start.elapsed().as_millis() as u64;
        timings.log_summary(0, "Stream search complete (no results)");
        return Ok(StreamSearchResponse {
            options: Vec::new(),
            errors,
        });
    }

    // 5. Build StreamOption results.
    // Use first provider as the default for all results.
    let default_provider = &providers[0];

    // Fold in what we already know about each hash on this provider: drop the
    // ones RD refuses outright, and rank the rest by how likely playback is to
    // actually start. The primary query was kicked off after Tier 1 dedup; await
    // it here and issue a supplementary query for hashes added since (Cardigann).
    let dead_hashes_await_start = Instant::now();
    let mut known: std::collections::HashMap<String, KnownAvailability> =
        dead_hashes_handle.await.unwrap_or_default();

    let initial_inputs: std::collections::HashSet<String> =
        initial_dead_hash_inputs.into_iter().collect();
    let final_hashes: std::collections::HashSet<String> = scored
        .iter()
        .filter(|pr| pr.is_candidate() && pr.release.protocol == "torrent")
        .filter_map(find_info_hash_for_result)
        .map(|h| h.to_lowercase())
        .collect();
    let delta: Vec<String> = final_hashes.difference(&initial_inputs).cloned().collect();
    timings.dead_hashes_supplementary_count = delta.len();
    if !delta.is_empty() {
        known.extend(known_availability(ctx.pool, default_provider.id, &delta).await);
    }
    timings.dead_hashes_ms = dead_hashes_await_start.elapsed().as_millis() as u64;

    // Fold in Torrentio's cache view for hashes we have no first-hand result
    // for. It is an aggregate across its user base rather than a check against
    // this account, so it is a good prior but not authoritative: our own probe
    // outcomes always win, and a Torrentio "not cached" only demotes (to Dead),
    // never blocks — the release may simply be new to their index.
    let mut torrentio_hints = 0usize;
    for (hash, cached) in &torrentio_cached {
        if known.contains_key(hash) {
            continue;
        }
        known.insert(
            hash.clone(),
            if *cached {
                KnownAvailability::LikelyAvailable
            } else {
                KnownAvailability::LikelyDead
            },
        );
        torrentio_hints += 1;
    }

    if !known.is_empty() {
        let blocked = known
            .values()
            .filter(|v| **v == KnownAvailability::Blocked)
            .count();
        let available = known
            .values()
            .filter(|v| **v == KnownAvailability::Available)
            .count();
        let likely_available = known
            .values()
            .filter(|v| **v == KnownAvailability::LikelyAvailable)
            .count();
        info!(
            known = known.len(),
            blocked,
            available,
            likely_available,
            torrentio_hints,
            "Applied known debrid availability to stream results"
        );
    }

    let options_build_start = Instant::now();
    let mut ranked: Vec<RankCandidate> = Vec::new();
    let mut seen_hashes: std::collections::HashSet<String> = std::collections::HashSet::new();

    // Build options from candidates only; rejects are only kept in `scored`
    // for decision-log purposes above. Score order here is what rank_options
    // falls back to, and it decides which duplicate of a hash+file survives.
    let mut candidates: Vec<&crate::watch::ProcessedRelease> =
        scored.iter().filter(|pr| pr.is_candidate()).collect();
    candidates.sort_by(|a, b| {
        b.total_score
            .partial_cmp(&a.total_score)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    for result in &candidates {
        if result.release.protocol == "torrent" {
            if let Some(hash) = find_info_hash_for_result(result) {
                let normalized = hash.to_lowercase();

                // Extract file_idx from guid pattern "hashidx_{hash}_f{idx}"
                let file_idx = extract_file_idx_from_guid(&result.release.guid);

                // Deduplicate by hash+file_idx — same torrent can appear
                // multiple times if different files match (different episodes)
                let dedup_key = if let Some(idx) = file_idx {
                    format!("{normalized}_f{idx}")
                } else {
                    normalized.clone()
                };
                if !seen_hashes.insert(dedup_key) {
                    continue;
                }

                let availability = known
                    .get(&normalized)
                    .copied()
                    .unwrap_or(KnownAvailability::Unknown);

                let match_score = result.match_score.unwrap_or(0.0);
                let quality_score = result.quality_score.unwrap_or(0.0);
                let total_score = result.total_score;

                let magnet_url = result
                    .release
                    .magnet_url
                    .clone()
                    .unwrap_or_else(|| format!("magnet:?xt=urn:btih:{normalized}"));

                // Populate files with file_idx for debrid file selection
                let files = if let Some(idx) = file_idx {
                    vec![nats_jobs::types::StreamFile {
                        id: idx,
                        path: result.release.file_name.clone().unwrap_or_default(),
                        size: result.release.size,
                    }]
                } else {
                    Vec::new()
                };

                let id = if let Some(idx) = file_idx {
                    format!("torrent_{normalized}_f{idx}_{}", default_provider.id)
                } else {
                    format!("torrent_{normalized}_{}", default_provider.id)
                };

                let quality = result
                    .parsed_quality
                    .as_ref()
                    .and_then(|pq| serde_json::to_value(pq).ok());

                let binge_group = binge_groups.get(&normalized).cloned();
                let option = StreamOption {
                    id,
                    release_title: result.release.title.clone(),
                    size: result.release.size,
                    indexer: result.release.indexer.clone(),
                    info_hash: Some(normalized),
                    nzb_url: None,
                    magnet_url: Some(magnet_url),
                    quality,
                    total_score,
                    match_score,
                    quality_score,
                    debrid_provider: default_provider.provider_type.clone(),
                    debrid_provider_id: default_provider.id,
                    cached: false,
                    source_type: StreamSourceType::Torrent,
                    files,
                    publish_date: Some(result.release.publish_date.clone()),
                    binge_group,
                    playback: None,
                    availability: None,
                    audio_languages: Vec::new(),
                };
                ranked.push(RankCandidate {
                    option,
                    availability,
                });
            }
        } else if has_usenet_provider
            && result.release.protocol == "usenet"
            && let Some(ref nzb_url) = result.release.download_url
            && let Some(provider) = providers.iter().find(|p| p.provider_type == "torbox")
        {
            let match_score = result.match_score.unwrap_or(0.0);
            let quality_score = result.quality_score.unwrap_or(0.0);
            let total_score = result.total_score;

            let quality = result
                .parsed_quality
                .as_ref()
                .and_then(|pq| serde_json::to_value(pq).ok());

            let option = StreamOption {
                id: format!("usenet_{}_{}", result.release.guid, provider.id),
                release_title: result.release.title.clone(),
                size: result.release.size,
                indexer: result.release.indexer.clone(),
                info_hash: None,
                nzb_url: Some(nzb_url.clone()),
                magnet_url: None,
                quality,
                total_score,
                match_score,
                quality_score,
                debrid_provider: provider.provider_type.clone(),
                debrid_provider_id: provider.id,
                cached: false,
                source_type: StreamSourceType::Usenet,
                files: Vec::new(),
                publish_date: Some(result.release.publish_date.clone()),
                binge_group: None,
                playback: None,
                availability: None,
                audio_languages: Vec::new(),
            };
            ranked.push(RankCandidate {
                option,
                availability: KnownAvailability::Unknown,
            });
        }
    }

    let rank_provider = RankProvider {
        id: default_provider.id,
    };
    let preferences = RankPreferences {
        prefer: request.prefer.as_ref(),
        audio_languages: &request.audio_languages,
    };
    let mut options = rank_options(ctx.pool, &rank_provider, ranked, &preferences).await;
    options.truncate(limit as usize);
    timings.options_build_ms = options_build_start.elapsed().as_millis() as u64;

    // Mark results included in final options as best
    if let Some(ref log) = collector {
        for opt in &options {
            log.mark_best_by_title(&opt.release_title);
        }
        let outcome = if options.is_empty() {
            "no_results"
        } else {
            "success"
        };
        log.set_outcome(outcome);
    }

    timings.total_ms = search_start.elapsed().as_millis() as u64;
    timings.log_summary(options.len(), "Stream search complete");

    // Auto-index new hashes from live search into the hash index (background)
    background_index_releases(ctx.pool.clone(), releases);

    if let Some(c) = collector {
        decision_log::spawn_flush(ctx.pool.clone(), c.into_flush_payload());
    }

    // Proactively warm the top results on the debrid provider so resolve is
    // near-instant when the user clicks play.
    if !options.is_empty() {
        // Pass the full list; warm_cache applies its own WARM_TOP_N and the
        // provider's free-slot budget. Truncating here would silently cap
        // warming below the client's auto-fallback depth.
        let warm_options: Vec<_> = options.clone();
        let pool = ctx.pool.clone();
        let http_client = ctx.http_client.clone();
        let providers = ctx.debrid_providers_arc.clone();
        let events = ctx.download_events.clone();
        tokio::spawn(async move {
            super::warm_cache::warm_top_results(pool, http_client, providers, events, warm_options)
                .await;
        });
    }

    Ok(StreamSearchResponse { options, errors })
}

/// Search indexers and score results, storing in cache on success.
async fn search_and_score_indexers(
    ctx: &StreamSearchContext<'_>,
    request: &StreamSearchRequest,
    debrid_sources: &[watch_repo::IndexerSourceRow],
    cache_key: &str,
    naming: &TvNaming,
) -> Result<(Vec<crate::watch::ProcessedRelease>, Vec<Release>), StreamSearchError> {
    let query = build_search_query(request);
    let indexer_ctx = IndexerContext {
        pool: ctx.pool,
        http_client: ctx.http_client,
    };

    let releases = search_debrid_sources(
        &indexer_ctx,
        ctx.indexers,
        ctx.pool,
        debrid_sources,
        &query,
        request,
    )
    .await;

    let releases = match releases {
        Ok(r) => r,
        Err(e) => {
            return Err(StreamSearchError::SearchFailed(e));
        }
    };

    if releases.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }

    info!(
        release_count = releases.len(),
        "Indexer search returned releases"
    );

    // Log sample of releases for debugging
    for r in releases.iter().take(3) {
        info!(
            title = %r.title,
            protocol = %r.protocol,
            info_hash = ?r.info_hash,
            magnet = r.magnet_url.is_some(),
            "Sample release"
        );
    }

    let patterns = build_patterns(request, naming);
    info!(patterns = ?patterns, "Scoring patterns");

    // Use explicit quality_profile_id if provided, otherwise fall back to
    // the default profile for the media kind (same as the Want flow).
    let quality_profile = if let Some(qp_id) = request.quality_profile_id {
        quality_profiles::get_quality_profile_by_id(ctx.pool, qp_id)
            .await
            .ok()
            .flatten()
    } else {
        // Determine media kind from request
        let kind = match request.media_type.as_deref().unwrap_or("movie") {
            "tv" => "tv",
            _ => "movie",
        };
        quality_profiles::get_default_for_media_kind(ctx.pool, kind)
            .await
            .ok()
            .flatten()
    };
    if let Some(ref qp) = quality_profile {
        info!(profile = %qp.name, max_res = ?qp.max_resolution, "Using quality profile");
    } else {
        info!("No quality profile found — scoring without resolution limits");
    }
    // Stream search uses lenient scoring: TRaSH filters disabled because public
    // trackers commonly have unnamed groups and x265 at HD which TRaSH rejects.
    // Quality scoring (resolution, codec, audio) still applies for ranking.
    let config = scoring_config(request, &patterns, quality_profile.as_ref(), None, naming);
    // Return the FULL scored set (candidates + rejects). Downstream filters
    // via `is_candidate()` where it needs candidates only; rejects are used
    // by the decision log so we don't have to re-score them.
    let scored = crate::services::want::scoring::score_batch(&releases, &config);

    let candidate_count = scored.iter().filter(|pr| pr.is_candidate()).count();
    info!(
        scored_count = scored.len(),
        candidate_count, "Scoring complete"
    );

    // Debug: if scoring returned 0 candidates, log why each release was rejected
    if candidate_count == 0 && !releases.is_empty() {
        warn!(
            release_count = releases.len(),
            "All releases rejected during scoring — check patterns and quality profile"
        );
    }

    // Store in cache
    INDEXER_RESULT_CACHE.insert(
        cache_key.to_string(),
        (scored.clone(), releases.clone(), Instant::now()),
    );

    // Evict expired entries periodically (simple inline eviction)
    if INDEXER_RESULT_CACHE.len() > 100 {
        INDEXER_RESULT_CACHE
            .retain(|_, (_, _, inserted_at)| inserted_at.elapsed() < INDEXER_CACHE_TTL);
    }

    Ok((scored, releases))
}

/// Streams rank everything by total score, so nothing is cut by a minimum
/// match score.
fn scoring_config<'a>(
    request: &'a StreamSearchRequest,
    patterns: &'a [String],
    quality_profile: Option<&'a crate::repositories::quality_profiles::QualityProfileRow>,
    trash_filters: Option<&'a crate::trash_filters::TrashFilterSet>,
    naming: &'a TvNaming,
) -> crate::services::want::scoring::ScoringConfig<'a> {
    let mut config = crate::services::want::scoring::ScoringConfig::new(
        &[],
        patterns,
        0.0,
        quality_profile,
        trash_filters,
    )
    .with_match_mode("any")
    .with_search_context(SearchContext::Structured);
    if let Some(ref mw) = request.media_want {
        config = config.with_media_want(mw);
    }
    naming.configure(config)
}

/// Extract the primary title from a MediaWant variant.
fn media_want_title(mw: &nats_jobs::MediaWant) -> String {
    match mw {
        nats_jobs::MediaWant::Book { title, .. }
        | nats_jobs::MediaWant::Audiobook { title, .. }
        | nats_jobs::MediaWant::Movie { title, .. }
        | nats_jobs::MediaWant::Game { title, .. } => title.clone(),
        nats_jobs::MediaWant::TvEpisode { show_name, .. }
        | nats_jobs::MediaWant::TvSeason { show_name, .. } => show_name.clone(),
        nats_jobs::MediaWant::Comic { title, .. } => title.clone(),
        nats_jobs::MediaWant::Music { artist, album, .. } => {
            if let Some(alb) = album {
                format!("{artist} {alb}")
            } else {
                artist.clone()
            }
        }
        nats_jobs::MediaWant::Software { name, .. } => name.clone(),
        nats_jobs::MediaWant::SportEvent {
            league_name,
            location,
            session_type,
            ..
        } => {
            let mut parts = vec![league_name.clone()];
            if let Some(loc) = location {
                parts.push(loc.clone());
            }
            if let Some(sess) = session_type {
                parts.push(sess.clone());
            }
            parts.join(" ")
        }
        nats_jobs::MediaWant::SportSeason {
            league_name,
            season,
            ..
        } => {
            format!("{league_name} {season}")
        }
    }
}

/// Extract external IDs from a MediaWant as (source_name, id_value) pairs.
fn media_want_external_ids(mw: &nats_jobs::MediaWant) -> Vec<(String, String)> {
    let mut ids = Vec::new();
    match mw {
        nats_jobs::MediaWant::TvEpisode { identifiers, .. }
        | nats_jobs::MediaWant::TvSeason { identifiers, .. } => {
            if let Some(tv_ids) = identifiers {
                if let Some(ref id) = tv_ids.imdb_id {
                    ids.push(("imdb".to_string(), id.clone()));
                }
                if let Some(ref id) = tv_ids.tmdb_id {
                    ids.push(("tmdb".to_string(), id.clone()));
                }
                if let Some(ref id) = tv_ids.tvdb_id {
                    ids.push(("tvdb".to_string(), id.clone()));
                }
            }
        }
        nats_jobs::MediaWant::Movie {
            identifiers: Some(movie_ids),
            ..
        } => {
            if let Some(ref id) = movie_ids.imdb_id {
                ids.push(("imdb".to_string(), id.clone()));
            }
            if let Some(ref id) = movie_ids.tmdb_id {
                ids.push(("tmdb".to_string(), id.clone()));
            }
        }
        _ => {}
    }
    ids
}

/// Build search query from request.
fn build_search_query(request: &StreamSearchRequest) -> String {
    if let Some(ref mw) = request.media_want {
        media_want_title(mw)
    } else {
        request.query.clone()
    }
}

/// Build patterns for scoring from request. Stream scoring matches by
/// substring, so every name the show goes by is a pattern.
fn build_patterns(request: &StreamSearchRequest, naming: &TvNaming) -> Vec<String> {
    if let Some(ref mw) = request.media_want {
        std::iter::once(media_want_title(mw))
            .chain(naming.show_aliases.iter().cloned())
            .chain(crate::scene_titles::alternate_titles_of(mw).iter().cloned())
            .collect()
    } else if !request.query.is_empty() {
        vec![request.query.clone()]
    } else {
        Vec::new()
    }
}

/// Find the info_hash for a raw Release.
/// Falls back to extracting hash from magnet URL if info_hash field is empty.
fn find_info_hash_for_release(release: &Release) -> Option<String> {
    if let Some(ref hash) = release.info_hash
        && !hash.is_empty()
    {
        return Some(hash.clone());
    }

    if let Some(ref magnet) = release.magnet_url {
        extract_hash_from_magnet(magnet)
    } else {
        None
    }
}

/// Find the info_hash for a scored result.
fn find_info_hash_for_result(result: &crate::watch::ProcessedRelease) -> Option<String> {
    find_info_hash_for_release(&result.release)
}

/// Collect lowercased unique info_hashes from torrent-protocol releases.
fn collect_torrent_hashes(releases: &[Release]) -> Vec<String> {
    releases
        .iter()
        .filter(|r| r.protocol == "torrent")
        .filter_map(find_info_hash_for_release)
        .map(|h| h.to_lowercase())
        .collect::<std::collections::HashSet<_>>()
        .into_iter()
        .collect()
}

/// Extract file_idx from a guid like "hashidx_{hash}_f{idx}".
fn extract_file_idx_from_guid(guid: &str) -> Option<i32> {
    let f_pos = guid.rfind("_f")?;
    guid[f_pos + 2..].parse().ok()
}

/// Extract info_hash from a magnet URL.
/// Looks for `xt=urn:btih:{hash}` parameter.
fn extract_hash_from_magnet(magnet: &str) -> Option<String> {
    for part in magnet.split('&') {
        let part = part.strip_prefix("magnet:?").unwrap_or(part);
        if let Some(value) = part.strip_prefix("xt=urn:btih:") {
            // Hash may be followed by more params
            let hash = value.split('&').next().unwrap_or(value);
            if !hash.is_empty() {
                return Some(hash.to_lowercase());
            }
        }
    }
    None
}

/// Load quality profile for stream search scoring.
async fn load_quality_profile(
    pool: &PgPool,
    request: &StreamSearchRequest,
) -> Option<crate::repositories::quality_profiles::QualityProfileRow> {
    if let Some(qp_id) = request.quality_profile_id {
        quality_profiles::get_quality_profile_by_id(pool, qp_id)
            .await
            .ok()
            .flatten()
    } else {
        let kind = match request.media_type.as_deref().unwrap_or("movie") {
            "tv" => "tv",
            _ => "movie",
        };
        quality_profiles::get_default_for_media_kind(pool, kind)
            .await
            .ok()
            .flatten()
    }
}

/// Search debrid-eligible sources using the indexer infrastructure.
async fn search_debrid_sources(
    indexer_ctx: &IndexerContext<'_>,
    indexers: &dyn IndexerResolver,
    pool: &PgPool,
    _sources: &[watch_repo::IndexerSourceRow],
    query: &str,
    request: &StreamSearchRequest,
) -> Result<Vec<Release>, String> {
    // Build external IDs from media_want if available
    let external_ids = if let Some(ref mw) = request.media_want {
        media_want_external_ids(mw)
            .into_iter()
            .filter_map(|(source, id)| {
                let source = match source.as_str() {
                    "imdb" => crate::watch::ExternalIdSource::Imdb,
                    "tmdb" => crate::watch::ExternalIdSource::Tmdb,
                    "tvdb" => crate::watch::ExternalIdSource::Tvdb,
                    "tvmaze" => crate::watch::ExternalIdSource::TvMaze,
                    _ => return None,
                };
                Some(crate::watch::ExternalId { source, id })
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };

    let (season, episode) = match request.media_want.as_ref() {
        Some(nats_jobs::MediaWant::TvEpisode {
            season, episode, ..
        }) => (Some(season.clone()), Some(episode.clone())),
        _ => (None, None),
    };

    let params = SearchIndexersParams {
        ctx: indexer_ctx,
        indexers,
        pool,
        query,
        limit: 100,
        external_ids: &external_ids,
        media_want: request.media_want.as_ref(),
        media_type_id: None,
        // 30s allows FlareSolverr time to solve Cloudflare challenges (~10-20s).
        // Non-Cloudflare sources respond in 1-2s so this doesn't slow them down.
        per_source_timeout: Some(Duration::from_secs(30)),
        season,
        episode,
    };

    search_indexers(&params).await
}

/// Load enabled debrid providers from the database.
async fn load_enabled_debrid_providers(
    pool: &PgPool,
) -> Result<Vec<DebridProviderRow>, StreamSearchError> {
    let rows = sqlx::query_as!(
        DebridProviderRow,
        r#"SELECT id, provider_type, api_key
           FROM debrid_providers
           WHERE enabled = true
           ORDER BY priority DESC, name"#,
    )
    .fetch_all(pool)
    .await
    .map_err(|e| StreamSearchError::Database(e.to_string()))?;

    Ok(rows)
}

// ---------------------------------------------------------------------------
// Hash Index integration
// ---------------------------------------------------------------------------

/// Search the local hash index for pre-indexed torrents.
/// Returns releases ready for the scoring pipeline.
/// When file data is available (from BitMagnet/Torrentio), creates one
/// release per file so season packs are scored as individual episodes.
async fn search_hash_index(pool: &PgPool, request: &StreamSearchRequest) -> Vec<Release> {
    // Extract IMDB ID from media_want identifiers
    let external_ids = request
        .media_want
        .as_ref()
        .map(media_want_external_ids)
        .unwrap_or_default();

    let imdb_id = external_ids
        .iter()
        .find(|(source, _)| source == "imdb")
        .map(|(_, id)| id.as_str());

    if let Some(imdb) = imdb_id {
        // Extract season/episode for filtered file lookup
        let (season, episode) = match request.media_want.as_ref() {
            Some(nats_jobs::MediaWant::TvEpisode {
                season, episode, ..
            }) => (season.parse::<i16>().ok(), episode.parse::<i16>().ok()),
            Some(nats_jobs::MediaWant::TvSeason { season, .. }) => {
                (season.parse::<i16>().ok(), None)
            }
            _ => (None, None),
        };

        let rows_with_files = hash_index::search_by_imdb_with_files(pool, imdb, season, episode)
            .await
            .unwrap_or_default();

        if !rows_with_files.is_empty() {
            info!(
                count = rows_with_files.len(),
                "Hash index returned results (with files)"
            );
        }

        hash_index_rows_with_files_to_releases(rows_with_files)
    } else {
        // Fallback to title search (no file unrolling — title search has no JOIN)
        let title = request
            .media_want
            .as_ref()
            .map(media_want_title)
            .unwrap_or_else(|| request.query.clone());

        if title.is_empty() {
            return Vec::new();
        }

        let rows = hash_index::search_by_title(pool, &title, None)
            .await
            .unwrap_or_default();

        if !rows.is_empty() {
            info!(count = rows.len(), "Hash index returned results");
        }

        hash_index_rows_to_releases(rows)
    }
}

/// Per-source result tracking from external API search, for decision logging.
struct ExternalApiStats {
    zilean_count: usize,
    zilean_query: String,
    zilean_used: bool,
    torrentio_count: usize,
    torrentio_query: String,
    torrentio_used: bool,
    eztv_count: usize,
    eztv_query: String,
    eztv_used: bool,
}

/// Handles for independently-spawned external API tasks.
/// Each source runs in its own tokio task and sends results via oneshot.
/// Background indexing happens inside each task regardless of whether
/// the receiver consumes the results.
struct ExternalApiHandles {
    torrentio: tokio::sync::oneshot::Receiver<Vec<crate::hash_sources::torrentio::TorrentioResult>>,
    zilean: tokio::sync::oneshot::Receiver<Vec<HashIndexEntry>>,
    eztv: tokio::sync::oneshot::Receiver<Vec<HashIndexEntry>>,
    torrentio_query: String,
    zilean_query: String,
    eztv_query: String,
}

/// Dedup releases by info_hash (+ file_idx from guid), keeping the entry with
/// richer metadata. This avoids scoring the same torrent twice when it appears
/// in both the hash index and an external source.
fn dedup_releases(releases: &mut Vec<Release>) {
    let mut seen: std::collections::HashMap<String, usize> =
        std::collections::HashMap::with_capacity(releases.len());
    let mut keep = Vec::with_capacity(releases.len());

    for release in releases.drain(..) {
        let dedup_key = if let Some(ref hash) = release.info_hash {
            // Include file_idx if encoded in guid (e.g. "hashidx_abc123_f3")
            if let Some(suffix) = release.guid.strip_prefix("hashidx_") {
                suffix.to_string()
            } else {
                hash.to_lowercase()
            }
        } else {
            // No hash — can't dedup, always keep
            keep.push(release);
            continue;
        };

        if let Some(&idx) = seen.get(&dedup_key) {
            // Prefer the entry with more metadata (size, seeders)
            let existing = &keep[idx];
            let existing_richness =
                (existing.size > 0) as u8 + (existing.seeders.unwrap_or(0) > 0) as u8;
            let new_richness = (release.size > 0) as u8 + (release.seeders.unwrap_or(0) > 0) as u8;
            if new_richness > existing_richness {
                keep[idx] = release;
            }
        } else {
            seen.insert(dedup_key, keep.len());
            keep.push(release);
        }
    }

    *releases = keep;
}

/// Convert external API entries (HashIndexEntry + optional TorrentioResult file data)
/// into Release structs for the scoring pipeline.
fn external_entries_to_releases(
    entries: &[HashIndexEntry],
    torrentio_files: &[HashFileEntry],
) -> Vec<Release> {
    if entries.is_empty() {
        return Vec::new();
    }

    let rows: Vec<HashIndexRow> = entries
        .iter()
        .map(|e| HashIndexRow {
            info_hash: e.info_hash.clone(),
            imdb_id: e.imdb_id.clone(),
            tmdb_id: e.tmdb_id.clone(),
            original_title: e.original_title.clone(),
            parsed_title: e.parsed_title.clone(),
            year: e.year,
            quality: e.quality.clone(),
            size: e.size,
            magnet_url: e.magnet_url.clone(),
            source: e.source.clone(),
            seeders: e.seeders,
            season: e.season,
            episodes: e.episodes.clone(),
        })
        .collect();

    let mut releases = hash_index_rows_to_releases(rows);

    // Encode file_idx into guid for Torrentio releases that have file data,
    // so it can be threaded through to StreamOption for debrid file selection.
    for release in &mut releases {
        if let Some(ref hash) = release.info_hash
            && let Some(file) = torrentio_files.iter().find(|f| f.info_hash == *hash)
        {
            release.guid = format!("hashidx_{}_f{}", hash, file.file_idx);
        }
    }

    releases
}

/// Spawn external API searches (Torrentio, StremThru, EZTV) as independent tasks.
/// Each task auto-indexes results into the hash index regardless of whether
/// the caller uses them. Returns handles with oneshot receivers.
fn spawn_external_apis(
    http_client: &reqwest::Client,
    pool: &PgPool,
    request: &StreamSearchRequest,
    real_debrid_key: Option<String>,
) -> ExternalApiHandles {
    let external_ids = request
        .media_want
        .as_ref()
        .map(media_want_external_ids)
        .unwrap_or_default();

    let imdb_id = external_ids
        .iter()
        .find(|(source, _)| source == "imdb")
        .map(|(_, id)| id.clone());

    let query = request
        .media_want
        .as_ref()
        .map(media_want_title)
        .unwrap_or_else(|| request.query.clone());

    let is_tv = matches!(request.media_type.as_deref(), Some("tv"));

    let (season, episode) = if is_tv {
        let (s, e) = extract_season_episode(request);
        (
            s.and_then(|v| v.parse::<u32>().ok()),
            e.and_then(|v| v.parse::<u32>().ok()),
        )
    } else {
        (None, None)
    };

    // Build query strings for logging
    let torrentio_query = if let Some(ref imdb) = imdb_id {
        if is_tv {
            if let (Some(s), Some(e)) = (season, episode) {
                format!("stream/series/{imdb}:{s}:{e}.json")
            } else {
                format!("stream/series/{imdb}.json")
            }
        } else {
            format!("stream/movie/{imdb}.json")
        }
    } else {
        String::new()
    };

    let zilean_query = if is_tv {
        if let (Some(s), Some(e)) = (season, episode) {
            format!("{query} S{s:02}E{e:02}")
        } else if let Some(s) = season {
            format!("{query} S{s:02}")
        } else {
            query.clone()
        }
    } else {
        query.clone()
    };

    let eztv_query = imdb_id.clone().unwrap_or_default();

    // Spawn Torrentio
    let (torrentio_tx, torrentio_rx) = tokio::sync::oneshot::channel();
    {
        let client = http_client.clone();
        let pool = pool.clone();
        let imdb = imdb_id.clone();
        tokio::spawn(async move {
            let torrentio = TorrentioClient::new(client).with_real_debrid_key(real_debrid_key);
            let results = if let Some(imdb) = imdb.as_deref() {
                if is_tv {
                    if let (Some(s), Some(e)) = (season, episode) {
                        torrentio.search_series(imdb, s, e).await
                    } else {
                        Vec::new()
                    }
                } else {
                    torrentio.search_movie(imdb).await
                }
            } else {
                Vec::new()
            };
            // Background index regardless of whether caller receives
            if !results.is_empty() {
                let entries: Vec<HashIndexEntry> =
                    results.iter().map(|r| r.entry.clone()).collect();
                let files: Vec<HashFileEntry> =
                    results.iter().filter_map(|r| r.file.clone()).collect();
                background_index_entries(pool.clone(), entries);
                background_index_files(pool, files);
            }
            let _ = torrentio_tx.send(results);
        });
    }

    // Spawn Zilean/StremThru
    let (zilean_tx, zilean_rx) = tokio::sync::oneshot::channel();
    {
        let client = http_client.clone();
        let pool = pool.clone();
        let zq = zilean_query.clone();
        tokio::spawn(async move {
            let zilean = ZileanClient::new(client);
            let results = if zq.is_empty() {
                Vec::new()
            } else {
                zilean.search(&zq).await
            };
            if !results.is_empty() {
                background_index_entries(pool, results.clone());
            }
            let _ = zilean_tx.send(results);
        });
    }

    // Spawn EZTV
    let (eztv_tx, eztv_rx) = tokio::sync::oneshot::channel();
    {
        let client = http_client.clone();
        let pool = pool.clone();
        let imdb = imdb_id.clone();
        tokio::spawn(async move {
            let eztv = EztvClient::new(client);
            let results = if is_tv {
                if let Some(imdb) = imdb.as_deref() {
                    eztv.search_show(imdb).await
                } else {
                    Vec::new()
                }
            } else {
                Vec::new()
            };
            if !results.is_empty() {
                background_index_entries(pool, results.clone());
            }
            let _ = eztv_tx.send(results);
        });
    }

    ExternalApiHandles {
        torrentio: torrentio_rx,
        zilean: zilean_rx,
        eztv: eztv_rx,
        torrentio_query,
        zilean_query,
        eztv_query,
    }
}

/// Extract season and episode strings from a stream search request.
fn extract_season_episode(request: &StreamSearchRequest) -> (Option<String>, Option<String>) {
    if let Some(ref mw) = request.media_want {
        match mw {
            nats_jobs::MediaWant::TvEpisode {
                season, episode, ..
            } => (Some(season.clone()), Some(episode.clone())),
            _ => (None, None),
        }
    } else {
        (None, None)
    }
}

/// Convert hash index rows into Release structs for the scoring pipeline.
/// Also used by backlog search for debrid candidates.
pub fn hash_index_rows_to_releases(rows: Vec<HashIndexRow>) -> Vec<Release> {
    rows.into_iter()
        .map(|row| {
            let mut external_ids = Vec::new();
            if let Some(ref imdb) = row.imdb_id {
                external_ids.push(crate::watch::ExternalId {
                    source: crate::watch::ExternalIdSource::Imdb,
                    id: imdb.clone(),
                });
            }

            Release {
                guid: format!("hashidx_{}", row.info_hash),
                title: row.original_title.clone(),
                file_name: None,
                download_url: None,
                magnet_url: row
                    .magnet_url
                    .clone()
                    .or_else(|| Some(format!("magnet:?xt=urn:btih:{}", row.info_hash))),
                info_url: None,
                size: row.size.unwrap_or(0),
                publish_date: String::new(),
                category_ids: Vec::new(),
                indexer: format!("hash_index:{}", row.source),
                protocol: "torrent".to_string(),
                info_hash: Some(row.info_hash.clone()),
                files: None,
                seeders: row.seeders,
                peers: None,
                leechers: None,
                grabs: None,
                indexer_id: None,
                indexer_source_id: None,
                raw_data: serde_json::Value::Null,
                external_ids,
                media: None,
                votes: None,
            }
        })
        .collect()
}

/// Convert hash index rows with file data into releases.
/// For torrents WITH files: creates one Release per file, using the file's
/// filename as the release title. This lets season packs be scored as individual episodes.
/// For torrents WITHOUT files: creates one Release per torrent (same as hash_index_rows_to_releases).
pub fn hash_index_rows_with_files_to_releases(
    rows: Vec<hash_index::HashIndexRowWithFiles>,
) -> Vec<Release> {
    let mut releases = Vec::new();

    for row in rows {
        let mut external_ids = Vec::new();
        if let Some(ref imdb) = row.imdb_id {
            external_ids.push(crate::watch::ExternalId {
                source: crate::watch::ExternalIdSource::Imdb,
                id: imdb.clone(),
            });
        }

        let magnet_url = row
            .magnet_url
            .clone()
            .unwrap_or_else(|| format!("magnet:?xt=urn:btih:{}", row.info_hash));

        if row.files.is_empty() {
            // No file data. Emit a bare torrent-level release only if the
            // parent can be matched to a specific episode downstream — a
            // single-ep torrent whose file list never got enumerated, or a
            // parent-NULL row (recovered via file-level metadata elsewhere).
            //
            // A pack without file data is dropped: episode-level wants can't
            // pin a file, and the search SQL itself already excludes these
            // from episode-specific results (parent `episodes` must explicitly
            // contain the wanted ep, or a file must prove it). This guard
            // covers the season-only / TvSeason paths that still return
            // packs without enumerated files.
            let is_pack_without_files = row.season.is_some() && row.episodes.is_none();
            if is_pack_without_files {
                continue;
            }
            releases.push(Release {
                guid: format!("hashidx_{}", row.info_hash),
                title: row.original_title.clone(),
                file_name: None,
                download_url: None,
                magnet_url: Some(magnet_url),
                info_url: None,
                size: row.size.unwrap_or(0),
                publish_date: String::new(),
                category_ids: Vec::new(),
                indexer: format!("hash_index:{}", row.source),
                protocol: "torrent".to_string(),
                info_hash: Some(row.info_hash.clone()),
                files: None,
                seeders: row.seeders,
                peers: None,
                leechers: None,
                grabs: None,
                indexer_id: None,
                indexer_source_id: None,
                raw_data: serde_json::Value::Null,
                external_ids,
                media: None,
                votes: None,
            });
        } else {
            // File data available — one release per file
            for file in &row.files {
                let file_title = strip_file_extension(&file.filename);
                // If the file title is bare (no show/episode — e.g. "812 - Expert"
                // from a season pack), fall back to the torrent-level original_title
                // which may have been enriched during the original Torrentio search.
                let title = if is_bare_tv_title(&file_title) {
                    row.original_title.clone()
                } else {
                    file_title
                };
                releases.push(Release {
                    guid: format!("hashidx_{}_f{}", row.info_hash, file.file_idx),
                    title,
                    file_name: Some(file.filename.clone()),
                    download_url: None,
                    magnet_url: Some(magnet_url.clone()),
                    info_url: None,
                    size: file.size.unwrap_or(row.size.unwrap_or(0)),
                    publish_date: String::new(),
                    category_ids: Vec::new(),
                    indexer: format!("hash_index:{}", row.source),
                    protocol: "torrent".to_string(),
                    info_hash: Some(row.info_hash.clone()),
                    files: None,
                    seeders: row.seeders,
                    peers: None,
                    leechers: None,
                    grabs: None,
                    indexer_id: None,
                    indexer_source_id: None,
                    raw_data: serde_json::Value::Null,
                    external_ids: external_ids.clone(),
                    media: None,
                    votes: None,
                });
            }
        }
    }

    releases
}

/// Check if a title is "bare" for TV — no season and no episodes detected.
/// Used to detect hash index file entries like "812 - Expert" that are useless
/// for matching without the parent torrent's context.
fn is_bare_tv_title(title: &str) -> bool {
    let parsed = crate::parsing::parse_release(title, "tv");
    match &parsed.media {
        crate::parsing::ParsedMedia::Tv(tv) => tv.season.is_none() && tv.episodes.is_empty(),
        _ => true,
    }
}

/// Strip common video file extensions from a filename for use as a release title.
fn strip_file_extension(filename: &str) -> String {
    let extensions = [
        ".mkv", ".mp4", ".avi", ".wmv", ".m4v", ".ts", ".m2ts", ".flv", ".webm", ".mov",
    ];
    let lower = filename.to_lowercase();
    for ext in &extensions {
        if lower.ends_with(ext) {
            return filename[..filename.len() - ext.len()].to_string();
        }
    }
    filename.to_string()
}

/// Auto-index new hashes into the hash index in the background.
/// Call after stream search completes to grow the index organically.
fn background_index_releases(pool: PgPool, releases: Vec<Release>) {
    if releases.is_empty() {
        return;
    }

    tokio::spawn(async move {
        let entries: Vec<hash_index::HashIndexEntry> = releases
            .iter()
            .filter_map(|r| {
                let hash = r.info_hash.as_ref()?;
                if hash.is_empty() {
                    return None;
                }

                // Don't re-index things already from the hash index
                if r.indexer.starts_with("hash_index:") {
                    return None;
                }

                let parsed = parse_release(&r.title, "movies");
                let quality_json = serde_json::to_value(&parsed.quality).ok();
                let year = match &parsed.media {
                    crate::parsing::ParsedMedia::Movie(m) => {
                        m.year.and_then(|y| i16::try_from(y).ok())
                    }
                    _ => None,
                };

                // Extract IMDB ID from release external IDs
                let imdb_id = r
                    .external_ids
                    .iter()
                    .find(|eid| matches!(eid.source, crate::watch::ExternalIdSource::Imdb))
                    .map(|eid| eid.id.clone());

                let (season, episodes) = hash_index::derive_season_episodes(&r.title);

                Some(hash_index::HashIndexEntry {
                    info_hash: hash.to_lowercase(),
                    imdb_id,
                    tmdb_id: None,
                    original_title: r.title.clone(),
                    parsed_title: Some(parsed.title),
                    year,
                    quality: quality_json,
                    size: if r.size > 0 { Some(r.size) } else { None },
                    magnet_url: r.magnet_url.clone(),
                    source: "live_search".to_string(),
                    seeders: r.seeders,
                    season,
                    episodes,
                })
            })
            .collect();

        if !entries.is_empty() {
            let count = entries.len();
            if let Err(e) = hash_index::upsert_batch(&pool, &entries).await {
                warn!(error = %e, "Failed to auto-index search results");
            } else {
                info!(count, "Auto-indexed search results into hash index");
            }
        }
    });
}

/// Background-index Torrentio file entries into the torrent_hash_files table.
/// Extract identifiers from the request for logging.
/// Returns (imdb_id, tmdb_id, title, extra_info).
fn extract_request_ids(request: &StreamSearchRequest) -> (String, String, String, String) {
    use nats_jobs::types::MediaWant;

    let Some(mw) = &request.media_want else {
        return (
            String::new(),
            String::new(),
            request.query.clone(),
            String::new(),
        );
    };

    match mw {
        MediaWant::Movie {
            title,
            year,
            identifiers,
            ..
        } => {
            let imdb = identifiers
                .as_ref()
                .and_then(|i| i.imdb_id.clone())
                .unwrap_or_default();
            let tmdb = identifiers
                .as_ref()
                .and_then(|i| i.tmdb_id.clone())
                .unwrap_or_default();
            let extra = year.map(|y| format!("year={y}")).unwrap_or_default();
            (imdb, tmdb, title.clone(), extra)
        }
        MediaWant::TvEpisode {
            show_name,
            season,
            episode,
            year,
            identifiers,
            ..
        } => {
            let imdb = identifiers
                .as_ref()
                .and_then(|i| i.imdb_id.clone())
                .unwrap_or_default();
            let tmdb = identifiers
                .as_ref()
                .and_then(|i| i.tmdb_id.clone())
                .unwrap_or_default();
            let extra = format!(
                "S{season}E{episode}{}",
                year.map(|y| format!(" year={y}")).unwrap_or_default()
            );
            (imdb, tmdb, show_name.clone(), extra)
        }
        MediaWant::TvSeason {
            show_name,
            season,
            year,
            identifiers,
            ..
        } => {
            let imdb = identifiers
                .as_ref()
                .and_then(|i| i.imdb_id.clone())
                .unwrap_or_default();
            let tmdb = identifiers
                .as_ref()
                .and_then(|i| i.tmdb_id.clone())
                .unwrap_or_default();
            let extra = format!(
                "S{season}{}",
                year.map(|y| format!(" year={y}")).unwrap_or_default()
            );
            (imdb, tmdb, show_name.clone(), extra)
        }
        MediaWant::Book {
            title,
            author,
            identifiers,
            ..
        }
        | MediaWant::Audiobook {
            title,
            author,
            identifiers,
            ..
        } => {
            let extra = author.as_deref().unwrap_or("").to_string();
            let _ids = identifiers; // BookIdentifiers doesn't have imdb/tmdb
            (String::new(), String::new(), title.clone(), extra)
        }
        _ => (
            String::new(),
            String::new(),
            request.query.clone(),
            format!("{mw:?}"),
        ),
    }
}

/// Background-persist raw HashIndexEntry results from external APIs into
/// torrent_hash_index. This bypasses the Release conversion that previously
/// caused all entries to be filtered out by the hash_index: prefix check.
fn background_index_entries(pool: PgPool, entries: Vec<HashIndexEntry>) {
    if entries.is_empty() {
        return;
    }

    tokio::spawn(async move {
        let count = entries.len();
        if let Err(e) = hash_index::upsert_batch(&pool, &entries).await {
            warn!(error = %e, "Failed to auto-index external API entries");
        } else {
            info!(count, "Auto-indexed external API entries into hash index");
        }
    });
}

fn background_index_files(pool: PgPool, files: Vec<HashFileEntry>) {
    if files.is_empty() {
        return;
    }

    tokio::spawn(async move {
        let count = files.len();
        if let Err(e) = hash_index::upsert_files(&pool, &files).await {
            warn!(error = %e, "Failed to auto-index file entries");
        } else {
            info!(count, "Auto-indexed file entries into hash index");
        }
    });
}
