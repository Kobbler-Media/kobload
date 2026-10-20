//! Debrid stream cache warming — proactively resolves top search results
//! so that playback is near-instant when the user clicks play.
//!
//! After stream search returns, the top [`WARM_TOP_N`] results are submitted to
//! the debrid provider (`add_magnet` + `select_files` + `get_torrent`). If any
//! are already cached on the provider, they're immediately unrestricted and
//! stored as `Ready`. If none are cached, option #1 is polled every 5 seconds
//! until ready or timeout, and the rest are deleted again to free their slots.
//!
//! Every conclusive probe result is persisted to `debrid_availability_cache`,
//! which is what later searches rank on — warming doubles as the availability
//! oracle that Real-Debrid no longer provides (`instantAvailability` was
//! removed; it now returns error_code 37 "Disabled endpoint").
//!
//! On resolve, the warm cache is checked first — a `Ready` entry returns the
//! stream URL with zero API calls, and an `InProgress` entry skips `add_magnet`.

use std::sync::LazyLock;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use tokio::sync::broadcast;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::debrid::{
    DebridError, DebridFile, DebridProviderResolver, DebridProviderType, DebridTorrentStatus,
    ExistingTorrent,
};
use crate::services::stream::load_provider_config;
use crate::types::DownloadStreamEvent;
use nats_jobs::types::StreamOption;
use sqlx::PgPool;

const WARM_CACHE_TTL: Duration = Duration::from_secs(300);

/// A Ready entry holds an unrestricted link to a torrent we left on the account, and both
/// stay valid for hours. Keeping it that long is what lets a next-episode prewarm, fired
/// when the current episode starts, still be instant when that episode ends.
const READY_TTL: Duration = Duration::from_secs(3 * 3600);

/// How often to poll the debrid provider for the #1 option.
const POLL_INTERVAL: Duration = Duration::from_secs(5);

/// Maximum time to poll before giving up.
const POLL_TIMEOUT: Duration = Duration::from_secs(300);

/// How many top results to warm.
///
/// Kept at or above kobbler's auto-fallback depth (`MAX_AUTO_FALLBACK = 5` in
/// `useStreamPlayback.svelte.ts`): the client will silently walk this far down
/// the list when a stream fails to resolve, so anything shallower guarantees
/// the fallbacks land on unprobed, cold options.
const WARM_TOP_N: usize = 8;

/// Concurrent warm probes.
///
/// Each probe is 3-4 provider calls, so 4 in flight is a ~16-call burst. Against
/// Real-Debrid's 250 requests/minute (~4/sec sustained) that reliably tripped
/// the limiter within 250ms, and a rate-limited probe learns nothing. Two in
/// flight plus [`WARM_STAGGER`] keeps the burst inside the budget.
const WARM_CONCURRENCY: usize = 2;

/// Delay between starting successive probes, to spread the call burst.
const WARM_STAGGER: Duration = Duration::from_millis(300);

/// How long to wait before retrying a probe that was rate limited.
const WARM_RATE_LIMIT_BACKOFF: Duration = Duration::from_secs(2);

/// Active torrent slots to leave free for the download client's real grabs.
///
/// A premium account was measured at `limit: 100`, so this is not normally the
/// binding constraint — it exists to stop warming from starving real grabs when
/// the pool is unexpectedly tight (secondary sources report far lower limits on
/// some accounts, so the value is read at runtime rather than assumed).
const SLOT_HEADROOM: u32 = 5;

/// Maximum cache size before triggering cleanup.
const MAX_CACHE_SIZE: usize = 200;

/// Cache key: uniquely identifies a debrid stream by provider + hash + file.
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct WarmCacheKey {
    pub debrid_provider_id: Uuid,
    pub info_hash: String,
    pub file_id: Option<i32>,
}

/// Cache entry representing the warming state of a stream.
#[derive(Debug, Clone)]
pub enum WarmCacheEntry {
    /// Magnet submitted to provider, awaiting caching.
    InProgress {
        torrent_id: String,
        provider_type: DebridProviderType,
        started_at: Instant,
    },
    /// Stream URL resolved and ready for instant playback.
    Ready {
        stream_url: String,
        filename: String,
        filesize: i64,
        mime_type: Option<String>,
        expires_at: Option<String>,
        torrent_id: String,
        provider_type: DebridProviderType,
        resolved_at: Instant,
    },
    /// Warming failed (will be retried on next search).
    Failed { error: String, failed_at: Instant },
}

impl WarmCacheEntry {
    fn is_expired(&self) -> bool {
        match self {
            Self::InProgress { started_at, .. } => started_at.elapsed() >= WARM_CACHE_TTL,
            Self::Ready { resolved_at, .. } => resolved_at.elapsed() >= READY_TTL,
            Self::Failed { failed_at, .. } => failed_at.elapsed() >= WARM_CACHE_TTL,
        }
    }
}

static WARM_CACHE: LazyLock<DashMap<WarmCacheKey, WarmCacheEntry>> = LazyLock::new(DashMap::new);

/// Look up a warm cache entry, returning `None` if missing or expired.
pub fn lookup(key: &WarmCacheKey) -> Option<WarmCacheEntry> {
    let entry = WARM_CACHE.get(key)?;
    if entry.is_expired() {
        drop(entry);
        WARM_CACHE.remove(key);
        return None;
    }
    Some(entry.clone())
}

/// Insert or update a warm cache entry.
fn insert(key: WarmCacheKey, entry: WarmCacheEntry) {
    WARM_CACHE.insert(key, entry);

    // Lazy eviction when cache grows large
    if WARM_CACHE.len() > MAX_CACHE_SIZE {
        WARM_CACHE.retain(|_, v| !v.is_expired());
    }
}

/// Why a hash is unavailable on a provider. Drives the negative-cache TTL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnavailableReason {
    /// Provider refuses to serve this file (RD error_code 35 / "virus" status).
    /// Durable for this hash; the search-side TTL is `BLOCKED_TTL` in
    /// `services/stream/service.rs`.
    Blocked,
    /// No seeds, metadata never resolved, or warming timed out. Can recover.
    Dead,
}

impl UnavailableReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::Blocked => "blocked",
            Self::Dead => "dead",
        }
    }
}

/// Persist a resolve outcome to `debrid_availability_cache` so future searches
/// can rank known-good hashes up and drop known-bad ones.
///
/// Pass `None` for a successful probe; `Some(reason)` for a failure. Callers
/// must not record inconclusive failures (rate limits, auth errors, no free
/// torrent slot) — see [`DebridError::is_inconclusive`].
///
/// Returns whether the verdict is news: no earlier row, a different verdict, or one
/// older than [`VERDICT_FRESHNESS`]. Re-probing a hash every search must not count as
/// fresh evidence each time.
pub async fn record_availability(
    pool: &PgPool,
    info_hash: &str,
    provider_id: Uuid,
    unavailable: Option<UnavailableReason>,
) -> bool {
    let available = unavailable.is_none();
    let reason = unavailable.map(|r| r.as_str());
    // Every reader lowercases before it looks up, and the primary key is
    // (info_hash, provider_id) — an unnormalised write is a row nothing ever
    // matches, plus a duplicate of the one that would have.
    let info_hash = info_hash.to_lowercase();
    match sqlx::query!(
        r#"INSERT INTO debrid_availability_cache (info_hash, provider_id, available, reason, checked_at)
           VALUES ($1, $2, $3, $4, NOW())
           ON CONFLICT (info_hash, provider_id) DO UPDATE
           SET available = $3, reason = $4, checked_at = NOW()
           RETURNING (old.checked_at IS NULL
                      OR old.available IS DISTINCT FROM $3
                      OR old.reason IS DISTINCT FROM $4
                      OR old.checked_at < NOW() - make_interval(secs => $5)) AS "fresh!""#,
        info_hash,
        provider_id,
        available,
        reason,
        VERDICT_FRESHNESS.as_secs_f64(),
    )
    .fetch_one(pool)
    .await
    {
        Ok(row) => row.fresh,
        Err(e) => {
            warn!(error = %e, "Failed to record debrid availability");
            false
        }
    }
}

const VERDICT_FRESHNESS: Duration = Duration::from_secs(6 * 3600);

/// Remove a torrent we submitted purely to probe its cache status.
///
/// Warming adds magnets to find out whether the provider already has them.
/// Anything we aren't going to stream has to be removed again, or it occupies
/// an active-torrent slot until it finishes downloading — a handful of searches
/// is enough to fill the pool and make every subsequent add fail.
async fn cleanup_torrent(
    http_client: &reqwest::Client,
    handler: &dyn crate::debrid::DebridProvider,
    api_key: &str,
    torrent_id: &str,
    info_hash: &str,
    was_reused: bool,
) {
    // Only remove what this probe created. A reused entry predates the search —
    // it may be a completed download the user still wants, and deleting it would
    // turn a duplicate-avoidance win into data loss.
    if was_reused {
        debug!(info_hash = %info_hash, "Warm cache: leaving pre-existing torrent in place");
        return;
    }
    if let Err(e) = handler
        .delete_torrent(http_client, api_key, torrent_id)
        .await
    {
        debug!(info_hash = %info_hash, error = %e, "Warm cache: cleanup delete failed");
    }
}

/// Map a [`DebridError`] to the availability outcome that should be recorded.
///
/// Returns `None` when the error says nothing about the hash itself, in which
/// case nothing should be written to the cache.
pub fn reason_for_error(err: &crate::debrid::DebridError) -> Option<UnavailableReason> {
    if err.is_inconclusive() {
        return None;
    }
    if err.is_permanent_for_hash() {
        return Some(UnavailableReason::Blocked);
    }
    Some(UnavailableReason::Dead)
}

/// Warm the top N stream options by submitting magnets to the debrid provider.
///
/// Called as a background task after `stream_search()` returns. Probes run
/// concurrently (bounded by [`WARM_CONCURRENCY`]) because each is dominated by
/// provider round-trip latency, and the client's auto-fallback will walk
/// several options deep — warming only the first would leave the rest cold.
///
/// For each option:
/// 1. Skip if already in the warm cache
/// 2. `add_magnet` + `select_files` + `get_torrent`
/// 3. If cached on provider: `unrestrict_link`, store as Ready, emit SSE
/// 4. If not cached: record it, remove the torrent again, and — for #1 only —
///    keep it and poll until ready so "play anyway" has something to wait on.
///
/// Every conclusive outcome is written to `debrid_availability_cache`, which is
/// what lets later searches rank by real availability instead of guessing.
pub async fn warm_top_results(
    pool: PgPool,
    http_client: reqwest::Client,
    debrid_providers: std::sync::Arc<dyn DebridProviderResolver>,
    download_events: broadcast::Sender<DownloadStreamEvent>,
    options: Vec<StreamOption>,
) {
    let candidates: Vec<_> = options
        .into_iter()
        .filter(|opt| opt.info_hash.is_some()) // torrents only
        .take(WARM_TOP_N)
        .collect();

    if candidates.is_empty() {
        return;
    }

    // All options from one search share a provider, so resolve config once.
    let provider_id = candidates[0].debrid_provider_id;
    let provider_config = match load_provider_config(&pool, provider_id).await {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, "Warm cache: failed to load provider config");
            return;
        }
    };
    let Some(provider_type) = DebridProviderType::parse(&provider_config.provider_type) else {
        warn!(provider = %provider_config.provider_type, "Warm cache: unknown provider type");
        return;
    };
    let handler = debrid_providers.get(&provider_type);
    let api_key = provider_config.api_key.clone();

    // One probe per info hash. Options are deduped by (hash, file_idx), so a
    // season pack can appear several times over — same torrent, different
    // episode file. Warming each would fire concurrent add_magnet calls for one
    // hash, and since Real-Debrid does not dedup, every one of them lands as a
    // separate entry. Probing the hash once answers the question for all of
    // them: cache status is a property of the torrent, not the file.
    let mut seen_hashes = std::collections::HashSet::new();
    let candidates: Vec<_> = candidates
        .into_iter()
        .filter(|opt| match opt.info_hash {
            Some(ref h) => seen_hashes.insert(h.to_lowercase()),
            None => true,
        })
        .collect();

    // Respect the account's active-torrent budget. Probing costs a slot until
    // we delete the torrent again, so warming into a nearly-full pool just
    // produces a wave of "torrent already active" errors and starves the
    // download client of slots it needs for real grabs.
    let budget = match handler.active_slots(&http_client, &api_key).await {
        Ok(Some(slots)) => {
            let free = slots.available().saturating_sub(SLOT_HEADROOM) as usize;
            if free == 0 {
                info!(
                    current = slots.current,
                    limit = slots.limit,
                    "Warm cache: no free torrent slots, skipping warming"
                );
                return;
            }
            free.min(candidates.len())
        }
        // Provider doesn't report slots (or the call failed) — proceed with the
        // full set rather than blocking warming on a diagnostic endpoint.
        _ => candidates.len(),
    };

    let to_warm: Vec<_> = candidates.into_iter().take(budget).collect();

    // One listing up front, shared by every probe. Real-Debrid does not dedup:
    // re-adding a hash it already holds creates a second entry rather than
    // returning the first, so without this a repeated search silently doubles
    // the account's torrent list and burns a slot each time. Measured on a live
    // account, 21 of 100 entries were redundant copies of an existing hash.
    let existing = handler
        .find_existing_torrents(&http_client, &api_key)
        .await
        .unwrap_or_else(|e| {
            warn!(error = %e, "Warm cache: could not list existing torrents, may create duplicates");
            std::collections::HashMap::new()
        });

    info!(
        count = to_warm.len(),
        existing = existing.len(),
        provider = %provider_type,
        "Starting debrid cache warming"
    );

    let existing = std::sync::Arc::new(existing);
    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(WARM_CONCURRENCY));
    let mut tasks = Vec::with_capacity(to_warm.len());

    for (idx, option) in to_warm.into_iter().enumerate() {
        let semaphore = semaphore.clone();
        let pool = pool.clone();
        let http_client = http_client.clone();
        let debrid_providers = debrid_providers.clone();
        let download_events = download_events.clone();
        let api_key = api_key.clone();
        let existing = existing.clone();

        tasks.push(tokio::spawn(async move {
            let Ok(_permit) = semaphore.acquire().await else {
                return;
            };
            // Spread the burst: probes are cheap to delay and expensive to
            // retry, and the provider counts refused requests toward the limit.
            if idx > 0 {
                tokio::time::sleep(WARM_STAGGER).await;
            }
            warm_one(
                idx,
                option,
                provider_type,
                api_key,
                existing,
                pool,
                http_client,
                debrid_providers,
                download_events,
            )
            .await;
        }));
    }

    for task in tasks {
        let _ = task.await;
    }
}

/// Probe a single option: is it already cached on the provider?
#[allow(clippy::too_many_arguments)]
async fn warm_one(
    idx: usize,
    option: StreamOption,
    provider_type: DebridProviderType,
    api_key: String,
    existing: std::sync::Arc<std::collections::HashMap<String, ExistingTorrent>>,
    pool: PgPool,
    http_client: reqwest::Client,
    debrid_providers: std::sync::Arc<dyn DebridProviderResolver>,
    download_events: broadcast::Sender<DownloadStreamEvent>,
) {
    let Some(ref raw_hash) = option.info_hash else {
        return;
    };
    let info_hash = raw_hash.to_lowercase();
    let is_primary = idx == 0;

    let file_id = option.files.first().map(|f| f.id);
    let file_name = option
        .files
        .first()
        .map(|f| f.path.clone())
        .filter(|p| !p.is_empty());
    let key = WarmCacheKey {
        debrid_provider_id: option.debrid_provider_id,
        info_hash: info_hash.clone(),
        file_id,
    };

    // Skip if already warming or ready
    if lookup(&key).is_some() {
        debug!(info_hash = %info_hash, "Warm cache: already has entry, skipping");
        return;
    }

    let handler = debrid_providers.get(&provider_type);

    let magnet = option
        .magnet_url
        .clone()
        .unwrap_or_else(|| format!("magnet:?xt=urn:btih:{info_hash}"));

    // Reuse an entry the provider already holds for this hash rather than
    // adding a second copy. Besides avoiding duplicates, this is free: the
    // listing was fetched once for the whole batch, so a hit costs no call at
    // all where add_magnet would cost one plus a slot.
    let reused = existing.get(&info_hash);
    let was_reused = reused.is_some();
    if let Some(found) = reused {
        debug!(
            info_hash = %info_hash,
            torrent_id = %found.id,
            "Warm cache: reusing existing torrent instead of re-adding"
        );
    }

    let mut add_result = match reused {
        Some(found) => Ok(found.id.clone()),
        None => handler.add_magnet(&http_client, &api_key, &magnet).await,
    };

    // A rate-limited probe tells us nothing about the hash and would otherwise
    // leave a gap in the ranking, so back off once and try again.
    if matches!(add_result, Err(DebridError::RateLimited { .. })) {
        debug!(info_hash = %info_hash, "Warm cache: rate limited, backing off");
        tokio::time::sleep(WARM_RATE_LIMIT_BACKOFF).await;
        add_result = handler.add_magnet(&http_client, &api_key, &magnet).await;
    }

    let torrent_id = match add_result {
        Ok(id) => id,
        Err(e) => {
            // Only record outcomes that actually tell us something about this
            // hash — a rate limit or a full slot pool must not poison the cache.
            if let Some(reason) = reason_for_error(&e) {
                super::outcomes::record_probe(
                    &pool,
                    key.debrid_provider_id,
                    &info_hash,
                    Some(&option.release_title),
                    Some(reason),
                )
                .await;
            }
            warn!(info_hash = %info_hash, error = %e, "Warm cache: add_magnet failed");
            insert(
                key,
                WarmCacheEntry::Failed {
                    error: e.to_string(),
                    failed_at: Instant::now(),
                },
            );
            return;
        }
    };

    // Check torrent status — we need the file list to resolve correct provider file IDs.
    let torrent_result = handler
        .get_torrent(&http_client, &api_key, &torrent_id)
        .await;

    // If waiting for file selection, select files using filename matching against
    // the provider's file list (file_idx from Torrentio/hash index doesn't match
    // provider-assigned IDs — e.g. RD uses 1-based IDs).
    let torrent_result = match torrent_result {
        Ok(t) if matches!(t.status, DebridTorrentStatus::WaitingFileSelection) => {
            let select_ids = DebridFile::resolve_select_ids(&t.files, file_name.as_deref());
            info!(
                info_hash = %info_hash,
                select_ids = ?select_ids,
                "Warm cache: selecting files (WaitingFileSelection)"
            );
            if let Err(e) = handler
                .select_files(&http_client, &api_key, &torrent_id, &select_ids)
                .await
            {
                warn!(info_hash = %info_hash, error = %e, "Warm cache: select_files failed");
            }
            // Brief pause for provider to process the selection
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            handler
                .get_torrent(&http_client, &api_key, &torrent_id)
                .await
        }
        other => other,
    };

    let torrent = match torrent_result {
        Ok(t) => t,
        Err(e) => {
            if let Some(reason) = reason_for_error(&e) {
                super::outcomes::record_probe(
                    &pool,
                    key.debrid_provider_id,
                    &info_hash,
                    Some(&option.release_title),
                    Some(reason),
                )
                .await;
            }
            warn!(info_hash = %info_hash, error = %e, "Warm cache: get_torrent failed");
            cleanup_torrent(
                &http_client,
                handler.as_ref(),
                &api_key,
                &torrent_id,
                &info_hash,
                was_reused,
            )
            .await;
            insert(
                key,
                WarmCacheEntry::Failed {
                    error: e.to_string(),
                    failed_at: Instant::now(),
                },
            );
            return;
        }
    };

    // The provider refuses this file outright — record it so it never gets
    // offered again, and drop the torrent.
    if matches!(torrent.status, DebridTorrentStatus::Blocked) {
        info!(info_hash = %info_hash, idx, "Warm cache: provider refused (filter-gate)");
        super::outcomes::record_probe(
            &pool,
            key.debrid_provider_id,
            &info_hash,
            Some(&option.release_title),
            Some(UnavailableReason::Blocked),
        )
        .await;
        cleanup_torrent(
            &http_client,
            handler.as_ref(),
            &api_key,
            &torrent_id,
            &info_hash,
            was_reused,
        )
        .await;
        insert(
            key,
            WarmCacheEntry::Failed {
                error: "Provider refused this file".to_string(),
                failed_at: Instant::now(),
            },
        );
        return;
    }

    let is_ready = matches!(
        torrent.status,
        DebridTorrentStatus::Cached | DebridTorrentStatus::Ready
    ) && !torrent.links.is_empty();

    if is_ready {
        // Already cached — unrestrict and store
        let Some(link) = torrent.links.first() else {
            return;
        };
        match handler.unrestrict_link(&http_client, &api_key, link).await {
            Ok(unrestricted) => {
                let expires_at = unrestricted.expires_at.map(|t| {
                    t.format(&time::format_description::well_known::Rfc3339)
                        .unwrap_or_default()
                });
                let prefixed = provider_type.prefixed_id(&torrent_id);
                info!(
                    info_hash = %info_hash,
                    idx,
                    "Warm cache: immediately ready (cached on provider)"
                );
                insert(
                    key.clone(),
                    WarmCacheEntry::Ready {
                        stream_url: unrestricted.url,
                        filename: unrestricted.filename,
                        filesize: unrestricted.filesize,
                        mime_type: unrestricted.mime_type,
                        expires_at,
                        torrent_id: prefixed,
                        provider_type,
                        resolved_at: Instant::now(),
                    },
                );
                super::outcomes::record_probe(
                    &pool,
                    key.debrid_provider_id,
                    &info_hash,
                    Some(&option.release_title),
                    None,
                )
                .await;
                emit_warm_ready(&download_events, &key, &option.debrid_provider);
            }
            Err(e) => {
                if let Some(reason) = reason_for_error(&e) {
                    super::outcomes::record_probe(
                        &pool,
                        key.debrid_provider_id,
                        &info_hash,
                        Some(&option.release_title),
                        Some(reason),
                    )
                    .await;
                }
                warn!(info_hash = %info_hash, error = %e, "Warm cache: unrestrict failed");
                insert(
                    key,
                    WarmCacheEntry::Failed {
                        error: e.to_string(),
                        failed_at: Instant::now(),
                    },
                );
            }
        }
        return;
    }

    // Not cached on the provider.
    if is_primary {
        // Keep #1 downloading and poll it — the user may choose to wait.
        let prefixed = provider_type.prefixed_id(&torrent_id);
        insert(
            key.clone(),
            WarmCacheEntry::InProgress {
                torrent_id: prefixed,
                provider_type,
                started_at: Instant::now(),
            },
        );
        info!(
            info_hash = %info_hash,
            "Warm cache: #1 not cached, starting background poll"
        );
        // Detached: the poll runs for minutes, and it must not hold the
        // caller's concurrency permit (or delay the other probes) while it does.
        tokio::spawn(poll_warm_entry(
            key,
            torrent_id,
            provider_type,
            pool,
            http_client,
            debrid_providers,
            download_events,
            option.debrid_provider.clone(),
            option.release_title.clone(),
            file_name,
        ));
    } else {
        // We only wanted to know whether it was cached. It isn't — record that
        // and hand the slot back rather than leaving it to download unwatched.
        debug!(info_hash = %info_hash, idx, "Warm cache: not cached, releasing slot");
        super::outcomes::record_probe(
            &pool,
            key.debrid_provider_id,
            &info_hash,
            Some(&option.release_title),
            Some(UnavailableReason::Dead),
        )
        .await;
        cleanup_torrent(
            &http_client,
            handler.as_ref(),
            &api_key,
            &torrent_id,
            &info_hash,
            was_reused,
        )
        .await;
    }
}

/// Poll the debrid provider for option #1 until it becomes ready.
#[allow(clippy::too_many_arguments)]
async fn poll_warm_entry(
    key: WarmCacheKey,
    torrent_id: String,
    provider_type: DebridProviderType,
    pool: PgPool,
    http_client: reqwest::Client,
    debrid_providers: std::sync::Arc<dyn DebridProviderResolver>,
    download_events: broadcast::Sender<DownloadStreamEvent>,
    debrid_provider_name: String,
    release_title: String,
    file_name: Option<String>,
) {
    let deadline = Instant::now() + POLL_TIMEOUT;
    let handler = debrid_providers.get(&provider_type);

    let api_key = match load_provider_config(&pool, key.debrid_provider_id).await {
        Ok(c) => c.api_key,
        Err(e) => {
            warn!(error = %e, "Warm poll: failed to load provider config");
            return;
        }
    };

    loop {
        tokio::time::sleep(POLL_INTERVAL).await;

        if Instant::now() > deadline {
            info!(info_hash = %key.info_hash, "Warm poll: timed out");
            super::outcomes::record_probe(
                &pool,
                key.debrid_provider_id,
                &key.info_hash,
                Some(&release_title),
                Some(UnavailableReason::Dead),
            )
            .await;
            break;
        }

        // Check if entry was evicted or superseded
        if lookup(&key).is_none() {
            debug!(info_hash = %key.info_hash, "Warm poll: entry expired, stopping");
            break;
        }

        match handler
            .get_torrent(&http_client, &api_key, &torrent_id)
            .await
        {
            Ok(torrent) => {
                let is_ready = matches!(
                    torrent.status,
                    DebridTorrentStatus::Cached | DebridTorrentStatus::Ready
                ) && !torrent.links.is_empty();

                if is_ready {
                    if let Some(link) = torrent.links.first() {
                        match handler.unrestrict_link(&http_client, &api_key, link).await {
                            Ok(unrestricted) => {
                                let expires_at = unrestricted.expires_at.map(|t| {
                                    t.format(&time::format_description::well_known::Rfc3339)
                                        .unwrap_or_default()
                                });
                                let prefixed = provider_type.prefixed_id(&torrent_id);
                                info!(
                                    info_hash = %key.info_hash,
                                    "Warm poll: ready, unrestricted"
                                );
                                insert(
                                    key.clone(),
                                    WarmCacheEntry::Ready {
                                        stream_url: unrestricted.url,
                                        filename: unrestricted.filename,
                                        filesize: unrestricted.filesize,
                                        mime_type: unrestricted.mime_type,
                                        expires_at,
                                        torrent_id: prefixed,
                                        provider_type,
                                        resolved_at: Instant::now(),
                                    },
                                );
                                emit_warm_ready(&download_events, &key, &debrid_provider_name);
                                super::outcomes::record_probe(
                                    &pool,
                                    key.debrid_provider_id,
                                    &key.info_hash,
                                    Some(&release_title),
                                    None,
                                )
                                .await;
                            }
                            Err(e) => {
                                warn!(error = %e, "Warm poll: unrestrict failed");
                                insert(
                                    key,
                                    WarmCacheEntry::Failed {
                                        error: e.to_string(),
                                        failed_at: Instant::now(),
                                    },
                                );
                            }
                        }
                    }
                    return;
                }

                match torrent.status {
                    DebridTorrentStatus::Blocked => {
                        warn!(
                            info_hash = %key.info_hash,
                            "Warm poll: provider refused this file (filter-gate)"
                        );
                        super::outcomes::record_probe(
                            &pool,
                            key.debrid_provider_id,
                            &key.info_hash,
                            Some(&release_title),
                            Some(UnavailableReason::Blocked),
                        )
                        .await;
                        cleanup_torrent(
                            &http_client,
                            handler.as_ref(),
                            &api_key,
                            &torrent_id,
                            &key.info_hash,
                            false,
                        )
                        .await;
                        insert(
                            key,
                            WarmCacheEntry::Failed {
                                error: "Provider refused this file".to_string(),
                                failed_at: Instant::now(),
                            },
                        );
                        return;
                    }
                    DebridTorrentStatus::Error { ref message } => {
                        warn!(
                            info_hash = %key.info_hash,
                            error = %message,
                            "Warm poll: provider reported error"
                        );
                        super::outcomes::record_probe(
                            &pool,
                            key.debrid_provider_id,
                            &key.info_hash,
                            Some(&release_title),
                            Some(UnavailableReason::Dead),
                        )
                        .await;
                        cleanup_torrent(
                            &http_client,
                            handler.as_ref(),
                            &api_key,
                            &torrent_id,
                            &key.info_hash,
                            false,
                        )
                        .await;
                        insert(
                            key,
                            WarmCacheEntry::Failed {
                                error: message.clone(),
                                failed_at: Instant::now(),
                            },
                        );
                        return;
                    }
                    DebridTorrentStatus::WaitingFileSelection => {
                        let select_ids =
                            DebridFile::resolve_select_ids(&torrent.files, file_name.as_deref());
                        info!(
                            info_hash = %key.info_hash,
                            select_ids = ?select_ids,
                            "Warm poll: selecting files (WaitingFileSelection)"
                        );
                        if let Err(e) = handler
                            .select_files(&http_client, &api_key, &torrent_id, &select_ids)
                            .await
                        {
                            warn!(
                                info_hash = %key.info_hash,
                                error = %e,
                                "Warm poll: select_files failed"
                            );
                        }
                        // Continue polling — provider needs a moment to process the selection
                    }
                    DebridTorrentStatus::Downloading { progress } => {
                        debug!(
                            info_hash = %key.info_hash,
                            progress = %progress,
                            "Warm poll: still downloading"
                        );
                    }
                    _ => {}
                }
            }
            Err(e) => {
                warn!(
                    info_hash = %key.info_hash,
                    error = %e,
                    "Warm poll: get_torrent failed"
                );
                // Don't fail on transient errors — keep polling
            }
        }
    }
}

/// Emit a `StreamWarmReady` SSE event.
fn emit_warm_ready(
    download_events: &broadcast::Sender<DownloadStreamEvent>,
    key: &WarmCacheKey,
    debrid_provider: &str,
) {
    let _ = download_events.send(DownloadStreamEvent::StreamWarmReady {
        info_hash: key.info_hash.clone(),
        file_id: key.file_id,
        debrid_provider_id: key.debrid_provider_id,
        debrid_provider: debrid_provider.to_string(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ready_entries_outlive_a_prewarm_but_failures_do_not() {
        let aged = Instant::now() - Duration::from_secs(45 * 60);
        let key = |file_id| WarmCacheKey {
            debrid_provider_id: Uuid::new_v4(),
            info_hash: "ttlhash".to_string(),
            file_id: Some(file_id),
        };
        let ready = key(1);
        insert(
            ready.clone(),
            WarmCacheEntry::Ready {
                stream_url: "https://example.invalid/e2.mkv".to_string(),
                filename: "e2.mkv".to_string(),
                filesize: 1,
                mime_type: None,
                expires_at: None,
                torrent_id: "rd:1".to_string(),
                provider_type: DebridProviderType::RealDebrid,
                resolved_at: aged,
            },
        );
        let failed = key(2);
        insert(
            failed.clone(),
            WarmCacheEntry::Failed {
                error: "x".to_string(),
                failed_at: aged,
            },
        );
        assert!(
            lookup(&ready).is_some(),
            "a next-episode prewarm must still be ready when the episode ends"
        );
        assert!(lookup(&failed).is_none());
    }
}
