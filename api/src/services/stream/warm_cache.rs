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
//! stream URL with zero API calls. Probes and resolves share their provider work
//! through [`super::flight`].

use std::sync::LazyLock;
use std::time::{Duration, Instant};

use dashmap::DashMap;
use tokio::sync::broadcast;
use tracing::{debug, info, warn};
use uuid::Uuid;

use super::attempt_log::{AttemptLog, Origin, Started, Step};
use super::flight::{self, Acquired, FlightContext, Job, ReadyLink};
use crate::debrid::{
    DebridFile, DebridProvider, DebridProviderResolver, DebridProviderType, DebridTorrentStatus,
    Lane, Throttled,
};
use crate::repositories::stream_attempts::Change;
use crate::services::stream::load_provider_config;
use crate::types::DownloadStreamEvent;
use nats_jobs::types::{StreamGroup, StreamOption};
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

/// Concurrent warm probes. The account's gate sets the call rate; this keeps probes
/// finishing in ranked order rather than all at once.
const WARM_CONCURRENCY: usize = 2;

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

pub fn forget(key: &WarmCacheKey) {
    WARM_CACHE.remove(key);
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
    /// Provider refused this file (RD error_code 35 / "virus" status). RD does not refuse
    /// a hash every time, so it only demotes; the TTL is `BLOCKED_TTL` in `availability.rs`.
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

pub fn store_ready(
    key: WarmCacheKey,
    link: &ReadyLink,
    torrent_id: &str,
    provider_type: DebridProviderType,
) {
    insert(
        key,
        WarmCacheEntry::Ready {
            stream_url: link.stream_url.clone(),
            filename: link.filename.clone(),
            filesize: link.filesize,
            mime_type: link.mime_type.clone(),
            expires_at: link.expires_at.clone(),
            torrent_id: torrent_id.to_string(),
            provider_type,
            resolved_at: Instant::now(),
        },
    );
}

fn failed(key: WarmCacheKey, error: String) {
    insert(
        key,
        WarmCacheEntry::Failed {
            error,
            failed_at: Instant::now(),
        },
    );
}

/// Probes the top options in ranked order, so the releases a client will fall back to are
/// verified before it reaches them. Releases that already failed are left for a client to
/// try, not re-probed on every search. Every call waits behind any user resolve on the same
/// account (see [`crate::debrid::throttle`]), and a probe of a hash a user is resolving
/// joins that resolve instead of adding the magnet twice.
pub async fn warm_top_results(
    pool: PgPool,
    http_client: reqwest::Client,
    debrid_providers: std::sync::Arc<dyn DebridProviderResolver>,
    download_events: broadcast::Sender<DownloadStreamEvent>,
    options: Vec<StreamOption>,
) {
    // A season pack appears once per episode file; its cache status is the torrent's, so
    // one probe answers for all of them.
    let mut seen_hashes = std::collections::HashSet::new();
    let lead_uncached = options
        .first()
        .is_some_and(|o| o.group == Some(StreamGroup::Uncached));
    // An uncached release costs an add only to be deleted again, unless it leads: then
    // nothing better exists and starting its download now is the head start.
    let candidates: Vec<_> = options
        .into_iter()
        .enumerate()
        .filter(|(i, opt)| match opt.group {
            Some(StreamGroup::Failed) => false,
            Some(StreamGroup::Uncached) => *i == 0 && lead_uncached,
            _ => true,
        })
        .map(|(_, opt)| opt)
        .filter(|opt| {
            opt.info_hash
                .as_ref()
                .is_some_and(|h| seen_hashes.insert(h.to_lowercase()))
        })
        .take(WARM_TOP_N)
        .filter(|opt| warm_key(opt).is_some_and(|k| lookup(&k).is_none()))
        .collect();
    let Some(first) = candidates.first() else {
        return;
    };

    let provider_id = first.debrid_provider_id;
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
    let ctx = FlightContext {
        pool,
        http_client,
        provider: debrid_providers.get(&provider_type),
        provider_type,
        api_key: provider_config.api_key,
    };

    // Probing costs a slot until the delete lands; warming into a nearly-full pool starves
    // the download client of slots for real grabs.
    let handler = Throttled::new(ctx.provider.clone(), &ctx.api_key, Lane::background());
    let budget = match handler.active_slots(&ctx.http_client, &ctx.api_key).await {
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
        _ => candidates.len(),
    };

    info!(count = budget, provider = %provider_type, "Starting debrid cache warming");

    let semaphore = std::sync::Arc::new(tokio::sync::Semaphore::new(WARM_CONCURRENCY));
    let mut tasks = Vec::with_capacity(budget);
    for (idx, option) in candidates.into_iter().take(budget).enumerate() {
        let semaphore = semaphore.clone();
        let ctx = ctx.clone();
        let download_events = download_events.clone();
        tasks.push(tokio::spawn(async move {
            let Ok(_permit) = semaphore.acquire().await else {
                return;
            };
            warm_one(idx, option, ctx, download_events).await;
        }));
    }
    for task in tasks {
        let _ = task.await;
    }
}

fn warm_key(option: &StreamOption) -> Option<WarmCacheKey> {
    Some(WarmCacheKey {
        debrid_provider_id: option.debrid_provider_id,
        info_hash: option.info_hash.as_deref()?.to_lowercase(),
        file_id: option.files.first().map(|f| f.id),
    })
}

async fn warm_one(
    idx: usize,
    option: StreamOption,
    ctx: FlightContext,
    download_events: broadcast::Sender<DownloadStreamEvent>,
) {
    let Some(key) = warm_key(&option) else {
        return;
    };
    let info_hash = key.info_hash.clone();
    let file = option.files.first();
    if lookup(&key).is_some() {
        debug!(info_hash = %info_hash, "Warm cache: already has entry, skipping");
        return;
    }

    let file_name = file.map(|f| f.path.clone()).filter(|p| !p.is_empty());
    let log = AttemptLog::start(
        &ctx.pool,
        Started {
            provider_id: option.debrid_provider_id,
            origin: Origin::Warm,
            info_hash: &info_hash,
            file_id: key.file_id,
            release_title: Some(&option.release_title),
            file_name: file_name.as_deref(),
        },
    )
    .await;
    let job = Job {
        provider_id: option.debrid_provider_id,
        info_hash: info_hash.clone(),
        file_id: key.file_id,
        file_name,
        magnet: option
            .magnet_url
            .clone()
            .unwrap_or_else(|| format!("magnet:?xt=urn:btih:{info_hash}")),
        release_title: Some(option.release_title.clone()),
        want_link: true,
        log,
    };

    match flight::acquire(&ctx, job.clone(), Lane::background()).await {
        Acquired::Ready { link, torrent_id } => {
            info!(info_hash = %info_hash, idx, "Warm cache: ready (cached on provider)");
            store_ready(key.clone(), &link, &torrent_id, ctx.provider_type);
            emit_warm_ready(&download_events, &key, &option.debrid_provider);
        }
        Acquired::Pending { torrent_id, .. } if idx == 0 => {
            // #1 keeps downloading so "play anyway" has something to wait on.
            insert(
                key.clone(),
                WarmCacheEntry::InProgress {
                    torrent_id: ctx.provider_type.prefixed_id(&torrent_id),
                    provider_type: ctx.provider_type,
                    started_at: Instant::now(),
                },
            );
            info!(info_hash = %info_hash, "Warm cache: #1 not cached, starting background poll");
            tokio::spawn(poll_warm_entry(
                key,
                torrent_id,
                ctx,
                download_events,
                option.debrid_provider.clone(),
                job,
            ));
        }
        Acquired::Pending { torrent_id, reused } => {
            debug!(info_hash = %info_hash, idx, "Warm cache: not cached, releasing slot");
            let message = if reused {
                "Not cached; the torrent was already on the account, so it was left there"
            } else {
                "Not cached; probe deleted to free its slot"
            };
            job.log
                .record(
                    Step::Released,
                    message,
                    Change {
                        status: Some("uncached"),
                        ..Default::default()
                    },
                )
                .await;
            super::outcomes::record_probe(
                &ctx.pool,
                job.provider_id,
                &info_hash,
                job.release_title.as_deref(),
                Some(UnavailableReason::Dead),
            )
            .await;
            if !reused {
                flight::release_probe(&ctx, &job, &torrent_id).await;
            }
        }
        Acquired::Refused => {
            info!(info_hash = %info_hash, idx, "Warm cache: provider refused (filter-gate)");
            failed(key, "Provider refused this file".to_string());
        }
        Acquired::Failed { message } => {
            warn!(info_hash = %info_hash, error = %message, "Warm cache: probe failed");
            failed(key, message);
        }
    }
}

async fn poll_warm_entry(
    key: WarmCacheKey,
    torrent_id: String,
    ctx: FlightContext,
    download_events: broadcast::Sender<DownloadStreamEvent>,
    debrid_provider_name: String,
    job: Job,
) {
    let deadline = Instant::now() + POLL_TIMEOUT;
    let handler = Throttled::new(ctx.provider.clone(), &ctx.api_key, Lane::background());
    let record = |reason| {
        super::outcomes::record_probe(
            &ctx.pool,
            key.debrid_provider_id,
            &key.info_hash,
            job.release_title.as_deref(),
            reason,
        )
    };

    loop {
        tokio::time::sleep(POLL_INTERVAL).await;

        let timed_out = Instant::now() > deadline;
        if timed_out || lookup(&key).is_none() {
            if timed_out {
                info!(info_hash = %key.info_hash, "Warm poll: timed out");
                record(Some(UnavailableReason::Dead)).await;
            } else {
                debug!(info_hash = %key.info_hash, "Warm poll: entry expired, stopping");
            }
            job.log
                .record(
                    Step::TimedOut,
                    format!(
                        "Gave up after {}s; the provider had not cached it",
                        POLL_TIMEOUT.as_secs()
                    ),
                    Change::failed("Timed out waiting for the provider to cache it"),
                )
                .await;
            break;
        }

        let torrent = match handler
            .get_torrent(&ctx.http_client, &ctx.api_key, &torrent_id)
            .await
        {
            Ok(t) => t,
            Err(e) => {
                warn!(info_hash = %key.info_hash, error = %e, "Warm poll: get_torrent failed");
                continue;
            }
        };

        match torrent.status {
            DebridTorrentStatus::Cached | DebridTorrentStatus::Ready
                if !torrent.links.is_empty() =>
            {
                let link = torrent
                    .wanted_link(
                        job.file_name.as_deref(),
                        job.provider_file_id(ctx.provider_type),
                    )
                    .unwrap_or(&torrent.links[0]);
                match handler
                    .unrestrict_link(&ctx.http_client, &ctx.api_key, link)
                    .await
                {
                    Ok(u) => {
                        info!(info_hash = %key.info_hash, "Warm poll: ready, unrestricted");
                        let link = ReadyLink {
                            stream_url: u.url,
                            filename: u.filename,
                            filesize: u.filesize,
                            mime_type: u.mime_type,
                            expires_at: u.expires_at.map(|t| {
                                t.format(&time::format_description::well_known::Rfc3339)
                                    .unwrap_or_default()
                            }),
                        };
                        store_ready(
                            key.clone(),
                            &link,
                            &ctx.provider_type.prefixed_id(&torrent_id),
                            ctx.provider_type,
                        );
                        emit_warm_ready(&download_events, &key, &debrid_provider_name);
                        record(None).await;
                        job.log
                            .record(
                                Step::Ready,
                                format!("Cached (100%); link unrestricted: {}", link.filename),
                                Change {
                                    status: Some("ready"),
                                    progress: Some(1.0),
                                    ..Default::default()
                                },
                            )
                            .await;
                    }
                    Err(e) => {
                        warn!(error = %e, "Warm poll: unrestrict failed");
                        let message = format!("Failed to unrestrict link: {e}");
                        job.log
                            .record(Step::Failed, &message, Change::failed(&message))
                            .await;
                        failed(key, e.to_string());
                    }
                }
                return;
            }
            DebridTorrentStatus::Blocked | DebridTorrentStatus::Error { .. } => {
                let (reason, error) = match torrent.status {
                    DebridTorrentStatus::Error { message } => (UnavailableReason::Dead, message),
                    _ => (
                        UnavailableReason::Blocked,
                        "Provider refused this file".to_string(),
                    ),
                };
                warn!(info_hash = %key.info_hash, error = %error, "Warm poll: provider gave up on torrent");
                let (step, status) = match reason {
                    UnavailableReason::Blocked => (Step::Refused, "refused"),
                    UnavailableReason::Dead => (Step::Failed, "failed"),
                };
                job.log
                    .record(
                        step,
                        &error,
                        Change {
                            status: Some(status),
                            error: Some(error.clone()),
                            ..Default::default()
                        },
                    )
                    .await;
                record(Some(reason)).await;
                flight::release_probe(&ctx, &job, &torrent_id).await;
                failed(key, error);
                return;
            }
            DebridTorrentStatus::WaitingFileSelection => {
                let select_ids =
                    DebridFile::resolve_select_ids(&torrent.files, job.file_name.as_deref());
                if let Err(e) = handler
                    .select_files(&ctx.http_client, &ctx.api_key, &torrent_id, &select_ids)
                    .await
                {
                    warn!(info_hash = %key.info_hash, error = %e, "Warm poll: select_files failed");
                }
            }
            DebridTorrentStatus::Downloading { progress } => job.log.progress(progress).await,
            _ => {}
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
