//! Stream resolve service — converts a stream option into a direct playable URL.

use sqlx::PgPool;
use tracing::{debug, info};
use uuid::Uuid;

use crate::debrid::throttle;
use crate::debrid::{
    DebridProvider, DebridProviderResolver, DebridProviderType, DebridTorrentStatus, Lane,
    Throttled,
};
use nats_jobs::types::{StreamPriority, StreamResolveRequest, StreamResolveResponse};

use super::flight::{self, Acquired, FlightContext, Job, REFUSED_MESSAGE};
use super::service::StreamSearchError;
use super::warm_cache::{self, WarmCacheEntry, WarmCacheKey};

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
    let provider_config = load_provider_config(ctx.pool, request.debrid_provider_id).await?;

    let provider_type =
        DebridProviderType::parse(&provider_config.provider_type).ok_or_else(|| {
            StreamSearchError::DebridError(format!(
                "Unknown provider type: {}",
                provider_config.provider_type
            ))
        })?;

    let provider = ctx.debrid_providers.get(&provider_type);

    if let Some(ref info_hash) = request.info_hash {
        let flight = FlightContext {
            pool: ctx.pool.clone(),
            http_client: ctx.http_client.clone(),
            provider,
            provider_type,
            api_key: provider_config.api_key,
        };
        resolve_torrent(&flight, &info_hash.to_lowercase(), request).await
    } else if let Some(ref nzb_url) = request.nzb_url {
        let handler = Throttled::new(provider, &provider_config.api_key, lane(request));
        let _session = handler
            .lane()
            .is_user()
            .then(|| handler.gate().user_session());
        resolve_usenet(ctx, &handler, &provider_config.api_key, nzb_url).await
    } else {
        Err(StreamSearchError::DebridError(
            "Either info_hash or nzb_url must be provided".to_string(),
        ))
    }
}

fn lane(request: &StreamResolveRequest) -> Lane {
    match request.priority.unwrap_or_default() {
        StreamPriority::User => Lane::user(),
        StreamPriority::Warm => Lane::background(),
    }
}

async fn resolve_torrent(
    ctx: &FlightContext,
    info_hash: &str,
    request: &StreamResolveRequest,
) -> Result<StreamResolveResponse, StreamSearchError> {
    let warm_key = WarmCacheKey {
        debrid_provider_id: request.debrid_provider_id,
        info_hash: info_hash.to_string(),
        file_id: request.file_id,
    };
    if let Some(WarmCacheEntry::Ready {
        stream_url,
        filename,
        filesize,
        mime_type,
        expires_at,
        torrent_id,
        ..
    }) = warm_cache::lookup(&warm_key)
    {
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

    let lane = lane(request);
    let _session = lane
        .is_user()
        .then(|| throttle::gate(ctx.provider_type, &ctx.api_key).user_session());

    // Warming records refusals seconds after a search returns, but the client's list was
    // ordered before that; this keeps walking into them free.
    if is_known_blocked(&ctx.pool, info_hash, request.debrid_provider_id).await {
        info!(info_hash = %info_hash, "Resolve: hash is known-blocked, refusing without calling provider");
        return Err(StreamSearchError::DebridError(REFUSED_MESSAGE.to_string()));
    }

    flight::claim(request.debrid_provider_id, info_hash);
    let job = Job {
        provider_id: request.debrid_provider_id,
        info_hash: info_hash.to_string(),
        file_id: request.file_id,
        file_name: None,
        magnet: request
            .magnet_url
            .clone()
            .unwrap_or_else(|| format!("magnet:?xt=urn:btih:{info_hash}")),
        release_title: None,
        want_link: true,
    };
    match flight::acquire(ctx, job, lane).await {
        Acquired::Ready { link, torrent_id } => {
            warm_cache::store_ready(warm_key, &link, &torrent_id, ctx.provider_type);
            Ok(StreamResolveResponse {
                stream_url: link.stream_url,
                filename: link.filename,
                filesize: link.filesize,
                mime_type: link.mime_type,
                expires_at: link.expires_at,
                cached: true,
                torrent_id: Some(torrent_id),
            })
        }
        Acquired::Pending { torrent_id, .. } => {
            info!(torrent_id = %torrent_id, "Torrent not cached, returning for client-side polling");
            Ok(StreamResolveResponse {
                stream_url: String::new(),
                filename: info_hash.to_string(),
                filesize: 0,
                mime_type: None,
                expires_at: None,
                cached: false,
                torrent_id: Some(ctx.provider_type.prefixed_id(&torrent_id)),
            })
        }
        Acquired::Refused => Err(StreamSearchError::DebridError(REFUSED_MESSAGE.to_string())),
        Acquired::Failed { message } => Err(StreamSearchError::DebridError(message)),
    }
}

/// Resolve usenet content via TorBox: add NZB -> poll -> get link.
async fn resolve_usenet(
    ctx: &StreamResolveContext<'_>,
    handler: &dyn DebridProvider,
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
                return Err(StreamSearchError::DebridError(REFUSED_MESSAGE.to_string()));
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
