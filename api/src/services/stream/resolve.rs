//! Stream resolve service — converts a stream option into a direct playable URL.

use sqlx::PgPool;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::debrid::{DebridFile, DebridProviderResolver, DebridProviderType, DebridTorrentStatus};
use nats_jobs::types::{StreamResolveRequest, StreamResolveResponse};

use super::service::StreamSearchError;

/// Maximum time to wait for a torrent to become ready (120 seconds).
/// Uncached content may take time to download on the debrid provider.
const RESOLVE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// Poll interval when waiting for torrent readiness.
const POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(500);

/// Context dependencies for stream resolve.
pub struct StreamResolveContext<'a> {
    pub pool: &'a PgPool,
    pub http_client: &'a reqwest::Client,
    pub debrid_providers: &'a dyn DebridProviderResolver,
}

/// Debrid provider config from DB.
pub struct ProviderConfig {
    pub provider_type: String,
    pub api_key: String,
}

/// Resolve a stream option into a direct playable URL.
///
/// For torrents: add magnet -> select files -> unrestrict link.
/// For usenet (TorBox): add NZB -> poll until ready -> get download link.
pub async fn stream_resolve(
    ctx: &StreamResolveContext<'_>,
    request: &StreamResolveRequest,
) -> Result<StreamResolveResponse, StreamSearchError> {
    // 1. Load provider config from DB
    let provider_config = load_provider_config(ctx.pool, request.debrid_provider_id).await?;

    let provider_type =
        DebridProviderType::parse(&provider_config.provider_type).ok_or_else(|| {
            StreamSearchError::DebridError(format!(
                "Unknown provider type: {}",
                provider_config.provider_type
            ))
        })?;

    let handler = ctx.debrid_providers.get(&provider_type);
    let api_key = &provider_config.api_key;

    // 2. Route based on source type
    if let Some(ref info_hash) = request.info_hash {
        resolve_torrent(
            ctx,
            handler.as_ref(),
            api_key,
            info_hash,
            request,
            provider_type,
        )
        .await
    } else if let Some(ref nzb_url) = request.nzb_url {
        resolve_usenet(ctx, handler.as_ref(), api_key, nzb_url).await
    } else {
        Err(StreamSearchError::DebridError(
            "Either info_hash or nzb_url must be provided".to_string(),
        ))
    }
}

/// Resolve a torrent via debrid: magnet -> select files -> unrestrict.
///
/// Checks the warm cache first for a pre-resolved result:
/// - **Ready**: returns the stream URL immediately (0 API calls)
/// - **InProgress**: skips add_magnet, reuses the existing torrent_id (saves 2 API calls)
/// - **Miss/Failed**: falls through to the normal add_magnet flow
async fn resolve_torrent(
    ctx: &StreamResolveContext<'_>,
    handler: &dyn crate::debrid::DebridProvider,
    api_key: &str,
    info_hash: &str,
    request: &StreamResolveRequest,
    provider_type: DebridProviderType,
) -> Result<StreamResolveResponse, StreamSearchError> {
    // ── Warm cache fast-path ─────────────────────────────────────────────
    let warm_key = super::warm_cache::WarmCacheKey {
        debrid_provider_id: request.debrid_provider_id,
        info_hash: info_hash.to_lowercase(),
        file_id: request.file_id,
    };

    if let Some(entry) = super::warm_cache::lookup(&warm_key) {
        match entry {
            super::warm_cache::WarmCacheEntry::Ready {
                stream_url,
                filename,
                filesize,
                mime_type,
                expires_at,
                torrent_id,
                ..
            } => {
                info!(info_hash = %info_hash, "Warm cache hit (Ready) — returning pre-resolved stream URL");
                return Ok(StreamResolveResponse {
                    stream_url,
                    filename,
                    filesize,
                    mime_type,
                    expires_at,
                    cached: true,
                    torrent_id: Some(torrent_id),
                });
            }
            super::warm_cache::WarmCacheEntry::InProgress {
                torrent_id: ref prefixed,
                ..
            } => {
                // We already have a torrent_id from warming — skip add_magnet + select_files.
                // Parse the raw ID from the prefixed format, then jump to get_torrent.
                if let Some((_, raw_id)) = DebridProviderType::from_prefixed_id(prefixed) {
                    info!(
                        info_hash = %info_hash,
                        torrent_id = %raw_id,
                        "Warm cache hit (InProgress) — skipping add_magnet"
                    );
                    let torrent = handler
                        .get_torrent(ctx.http_client, api_key, raw_id)
                        .await
                        .map_err(|e| {
                            StreamSearchError::DebridError(format!(
                                "Failed to get torrent status: {e}"
                            ))
                        })?;

                    let mut torrent = torrent;

                    // If still waiting for file selection, resolve correct provider
                    // file IDs by matching filename against the provider's file list.
                    if matches!(torrent.status, DebridTorrentStatus::WaitingFileSelection) {
                        let file_name =
                            resolve_file_name(ctx.pool, info_hash, request.file_id).await;
                        let select_ids =
                            DebridFile::resolve_select_ids(&torrent.files, file_name.as_deref());
                        info!(
                            info_hash = %info_hash,
                            select_ids = ?select_ids,
                            "Resolve (InProgress): selecting files (WaitingFileSelection)"
                        );
                        if let Err(e) = handler
                            .select_files(ctx.http_client, api_key, raw_id, &select_ids)
                            .await
                        {
                            warn!(info_hash = %info_hash, error = %e, "select_files failed");
                        }
                        // Brief pause for provider to process, then re-check
                        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                        torrent = handler
                            .get_torrent(ctx.http_client, api_key, raw_id)
                            .await
                            .map_err(|e| {
                                StreamSearchError::DebridError(format!(
                                    "Failed to get torrent status after select_files: {e}"
                                ))
                            })?;
                    }

                    let is_ready = matches!(
                        torrent.status,
                        DebridTorrentStatus::Cached | DebridTorrentStatus::Ready
                    ) && !torrent.links.is_empty();

                    if !is_ready {
                        return Ok(StreamResolveResponse {
                            stream_url: String::new(),
                            filename: request.info_hash.clone().unwrap_or_default(),
                            filesize: 0,
                            mime_type: None,
                            expires_at: None,
                            cached: false,
                            torrent_id: Some(prefixed.clone()),
                        });
                    }

                    let download_link = torrent.links.first().ok_or_else(|| {
                        StreamSearchError::DebridError("No download links available".to_string())
                    })?;

                    let unrestricted = handler
                        .unrestrict_link(ctx.http_client, api_key, download_link)
                        .await
                        .map_err(|e| {
                            StreamSearchError::DebridError(format!(
                                "Failed to unrestrict link: {e}"
                            ))
                        })?;

                    let expires_at = unrestricted.expires_at.map(|t| {
                        t.format(&time::format_description::well_known::Rfc3339)
                            .unwrap_or_default()
                    });

                    return Ok(StreamResolveResponse {
                        stream_url: unrestricted.url,
                        filename: unrestricted.filename,
                        filesize: unrestricted.filesize,
                        mime_type: unrestricted.mime_type,
                        expires_at,
                        cached: true,
                        torrent_id: Some(prefixed.clone()),
                    });
                }
            }
            super::warm_cache::WarmCacheEntry::Failed { .. } => {
                // Fall through to normal path
            }
        }
    }

    // ── Normal resolve path (no warm cache hit) ──────────────────────────

    // Refuse hashes a previous probe proved the provider will not serve.
    // Warming records these seconds after a search returns, but the client's
    // option list was ordered before that, so without this check it walks
    // straight back into them -- each costing a round trip and a slice of the
    // rate-limit budget to be told "no" again.
    if is_known_blocked(ctx.pool, info_hash, request.debrid_provider_id).await {
        info!(info_hash = %info_hash, "Resolve: hash is known-blocked, refusing without calling provider");
        return Err(StreamSearchError::DebridError(
            "Real-Debrid refused this release. Try a different source.".to_string(),
        ));
    }

    let magnet = request
        .magnet_url
        .clone()
        .unwrap_or_else(|| format!("magnet:?xt=urn:btih:{info_hash}"));

    // Reuse a torrent the provider already holds for this hash.
    //
    // The warm cache above is in-memory with a short TTL, so it misses whenever
    // the user lingers on a page, or after a restart. Without this check those
    // resolves call add_magnet on a hash Real-Debrid already has — and RD
    // answers by creating a *second* entry rather than returning the first,
    // which is how the account accumulated duplicates in the first place.
    let existing = handler
        .find_existing_torrent(ctx.http_client, api_key, info_hash)
        .await
        .unwrap_or_else(|e| {
            warn!(info_hash = %info_hash, error = %e, "Failed to check for existing torrent");
            None
        });

    let torrent_id = if let Some(found) = existing {
        info!(
            info_hash = %info_hash,
            torrent_id = %found.id,
            "Reusing existing torrent on provider instead of re-adding"
        );
        found.id
    } else {
        info!(info_hash = %info_hash, "Adding magnet to debrid provider");

        // Add magnet.
        //
        // A failure here is the most valuable signal we get: the user clicked
        // play and the provider refused. Record it so the same release is not
        // offered again — without this, a filter-gated hash reappears at the
        // top of every subsequent search for the same title.
        match handler.add_magnet(ctx.http_client, api_key, &magnet).await {
            Ok(id) => id,
            Err(e) => {
                if let Some(reason) = super::warm_cache::reason_for_error(&e) {
                    super::warm_cache::record_availability(
                        ctx.pool,
                        info_hash,
                        request.debrid_provider_id,
                        Some(reason),
                    )
                    .await;
                }
                return Err(StreamSearchError::DebridError(format!(
                    "Failed to add magnet: {e}"
                )));
            }
        }
    };

    debug!(torrent_id = %torrent_id, "Torrent ready to inspect, checking status");

    let prefixed_torrent_id = provider_type.prefixed_id(&torrent_id);

    // Check torrent status — we need the file list to resolve correct provider file IDs.
    let mut torrent = handler
        .get_torrent(ctx.http_client, api_key, &torrent_id)
        .await
        .map_err(|e| {
            StreamSearchError::DebridError(format!("Failed to get torrent status: {e}"))
        })?;

    // If waiting for file selection, resolve correct provider file IDs by
    // matching filename from our hash index against the provider's file list.
    if matches!(torrent.status, DebridTorrentStatus::WaitingFileSelection) {
        let file_name = resolve_file_name(ctx.pool, info_hash, request.file_id).await;
        let select_ids = DebridFile::resolve_select_ids(&torrent.files, file_name.as_deref());
        info!(
            info_hash = %info_hash,
            select_ids = ?select_ids,
            "Resolve: selecting files (WaitingFileSelection)"
        );
        if let Err(e) = handler
            .select_files(ctx.http_client, api_key, &torrent_id, &select_ids)
            .await
        {
            warn!(info_hash = %info_hash, error = %e, "select_files failed");
        }
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        torrent = handler
            .get_torrent(ctx.http_client, api_key, &torrent_id)
            .await
            .map_err(|e| {
                StreamSearchError::DebridError(format!(
                    "Failed to get torrent status after select_files: {e}"
                ))
            })?;
    }

    // A refused torrent will never become ready, so fail fast rather than
    // handing the client a torrent_id to poll until it times out.
    if matches!(torrent.status, DebridTorrentStatus::Blocked) {
        super::warm_cache::record_availability(
            ctx.pool,
            info_hash,
            request.debrid_provider_id,
            Some(super::warm_cache::UnavailableReason::Blocked),
        )
        .await;
        if let Err(e) = handler
            .delete_torrent(ctx.http_client, api_key, &torrent_id)
            .await
        {
            debug!(torrent_id = %torrent_id, error = %e, "Failed to clean up blocked torrent");
        }
        return Err(StreamSearchError::DebridError(
            "Real-Debrid refused this release. Try a different source.".to_string(),
        ));
    }

    let is_ready = matches!(
        torrent.status,
        DebridTorrentStatus::Cached | DebridTorrentStatus::Ready
    ) && !torrent.links.is_empty();

    if !is_ready {
        // Not cached — return immediately so frontend can poll for progress
        info!(torrent_id = %torrent_id, "Torrent not cached, returning for client-side polling");
        return Ok(StreamResolveResponse {
            stream_url: String::new(),
            filename: request.info_hash.clone().unwrap_or_default(),
            filesize: 0,
            mime_type: None,
            expires_at: None,
            cached: false,
            torrent_id: Some(prefixed_torrent_id),
        });
    }

    // Torrent is cached/ready — unrestrict the link
    let download_link = torrent
        .links
        .first()
        .ok_or_else(|| StreamSearchError::DebridError("No download links available".to_string()))?;

    info!(torrent_id = %torrent_id, "Unrestricting download link (cached)");

    let unrestricted = handler
        .unrestrict_link(ctx.http_client, api_key, download_link)
        .await
        .map_err(|e| StreamSearchError::DebridError(format!("Failed to unrestrict link: {e}")))?;

    // Record the successful resolve — this is first-hand evidence the hash is
    // cached for this account, the strongest availability signal we have.
    super::warm_cache::record_availability(ctx.pool, info_hash, request.debrid_provider_id, None)
        .await;

    let expires_at = unrestricted.expires_at.map(|t| {
        t.format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default()
    });

    Ok(StreamResolveResponse {
        stream_url: unrestricted.url,
        filename: unrestricted.filename,
        filesize: unrestricted.filesize,
        mime_type: unrestricted.mime_type,
        expires_at,
        cached: true,
        torrent_id: Some(prefixed_torrent_id),
    })
}

/// Resolve usenet content via TorBox: add NZB -> poll -> get link.
async fn resolve_usenet(
    ctx: &StreamResolveContext<'_>,
    handler: &dyn crate::debrid::DebridProvider,
    api_key: &str,
    nzb_url: &str,
) -> Result<StreamResolveResponse, StreamSearchError> {
    info!("Adding NZB to debrid provider");

    let download_id = handler
        .add_nzb(ctx.http_client, api_key, nzb_url)
        .await
        .map_err(|e| StreamSearchError::DebridError(format!("Failed to add NZB: {e}")))?;

    debug!(download_id = %download_id, "NZB added, polling for readiness");

    // Poll until ready
    let deadline = tokio::time::Instant::now() + RESOLVE_TIMEOUT;
    loop {
        let status = handler
            .get_usenet_download(ctx.http_client, api_key, &download_id)
            .await
            .map_err(|e| {
                StreamSearchError::DebridError(format!("Failed to get usenet status: {e}"))
            })?;

        match status.status {
            DebridTorrentStatus::Ready | DebridTorrentStatus::Cached => {
                let link = status.links.first().ok_or_else(|| {
                    StreamSearchError::DebridError(
                        "Usenet download ready but no links available".to_string(),
                    )
                })?;

                // Unrestrict the link
                let unrestricted = handler
                    .unrestrict_link(ctx.http_client, api_key, link)
                    .await
                    .map_err(|e| {
                        StreamSearchError::DebridError(format!(
                            "Failed to unrestrict usenet link: {e}"
                        ))
                    })?;

                let expires_at = unrestricted.expires_at.map(|t| {
                    t.format(&time::format_description::well_known::Rfc3339)
                        .unwrap_or_default()
                });

                return Ok(StreamResolveResponse {
                    stream_url: unrestricted.url,
                    filename: unrestricted.filename,
                    filesize: unrestricted.filesize,
                    mime_type: unrestricted.mime_type,
                    expires_at,
                    cached: false,
                    torrent_id: Some(download_id.clone()),
                });
            }
            DebridTorrentStatus::Error { ref message } => {
                return Err(StreamSearchError::DebridError(format!(
                    "Usenet download failed: {message}"
                )));
            }
            DebridTorrentStatus::Downloading { progress } => {
                debug!(progress = %progress, "Usenet download in progress");
            }
            DebridTorrentStatus::WaitingFileSelection => {
                // Should not occur for usenet — treat as in-progress
                debug!("Usenet resolve: unexpected WaitingFileSelection, continuing poll");
            }
            // Refusal is permanent, so polling on would only burn the deadline.
            DebridTorrentStatus::Blocked => {
                return Err(StreamSearchError::DebridError(
                    "Real-Debrid refused this release. Try a different source.".to_string(),
                ));
            }
        }

        if tokio::time::Instant::now() > deadline {
            return Err(StreamSearchError::DebridError(
                "Timed out waiting for usenet download".to_string(),
            ));
        }

        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

/// Whether a hash is recorded as refused by this provider.
///
/// Only `blocked` counts: a `dead` hash may have picked up seeders since, and
/// the user explicitly asked for this one, so it is worth a retry.
async fn is_known_blocked(pool: &PgPool, info_hash: &str, provider_id: Uuid) -> bool {
    sqlx::query_scalar!(
        r#"SELECT true AS "blocked!" FROM debrid_availability_cache
           WHERE provider_id = $1
             AND info_hash = $2
             AND available = false
             AND reason = 'blocked'
             AND checked_at > NOW() - INTERVAL '30 days'"#,
        provider_id,
        info_hash.to_lowercase(),
    )
    .fetch_optional(pool)
    .await
    .ok()
    .flatten()
    .unwrap_or(false)
}

/// Load a debrid provider's config by ID.
pub async fn load_provider_config(
    pool: &PgPool,
    provider_id: Uuid,
) -> Result<ProviderConfig, StreamSearchError> {
    sqlx::query_as!(
        ProviderConfig,
        r#"SELECT provider_type, api_key
           FROM debrid_providers
           WHERE id = $1 AND enabled = true"#,
        provider_id,
    )
    .fetch_optional(pool)
    .await
    .map_err(|e| StreamSearchError::Database(e.to_string()))?
    .ok_or_else(|| {
        StreamSearchError::DebridError(format!(
            "Debrid provider {provider_id} not found or disabled"
        ))
    })
}

/// Look up the filename for a file_id (Torrentio/hash-index file_idx) from our
/// local hash index. Returns None if file_id is None or the file isn't indexed.
async fn resolve_file_name(pool: &PgPool, info_hash: &str, file_id: Option<i32>) -> Option<String> {
    let file_idx = file_id?;
    match crate::repositories::hash_index::get_file_name(pool, info_hash, file_idx).await {
        Ok(name) => name,
        Err(e) => {
            warn!(info_hash = %info_hash, file_idx, error = %e, "Failed to look up file name from hash index");
            None
        }
    }
}
