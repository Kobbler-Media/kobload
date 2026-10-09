use axum::http::StatusCode;
use sha2::{Digest, Sha256};
use tracing::info;
use uuid::Uuid;

use crate::download_clients::{DownloadClientError, DownloadClientResolver};
use crate::helpers::{broadcast_download_created, log_event};
use crate::indexers::{IndexerContext, IndexerResolver};
use crate::repositories::{
    clients as clients_repo, downloads as downloads_repo, watch as watch_repo,
};
use crate::types::DownloadStreamEvent;
use sqlx::PgPool;
use tokio::sync::broadcast;

#[derive(Debug, Clone, Default)]
pub struct DownloadUrlContext {
    pub base_url: Option<String>,
    pub auth_header: Option<(String, String)>,
    pub cookie: Option<String>,
    /// Per-indexer seeding overrides (take precedence over download client defaults).
    pub seed_ratio: Option<f64>,
    pub seed_time_minutes: Option<i32>,
}

#[derive(Debug, Clone)]
pub struct DownloadSubmission {
    pub source: String,
    pub protocol: String,
    pub name: String,
    pub download_url: String,
    pub download_client_id: Option<Uuid>,
    pub callback_url: Option<String>,
    pub auth_token: Option<String>,
    pub metadata: serde_json::Value,
    pub release_payload: Option<serde_json::Value>,
    pub api_key_id: Option<Uuid>,
    pub url_context: DownloadUrlContext,
    pub indexer_source_id: Option<Uuid>,
    pub quality_profile_id: Option<Uuid>,
    pub media_kind: Option<String>,
    /// Suppress this submission if an active (non-failed/cancelled) download for
    /// the same URL was created recently. Set on automated grab paths (watch,
    /// want) where two patterns or a re-search can race the same release; left
    /// off for user-initiated downloads, which may legitimately re-request.
    pub skip_duplicate_url: bool,
}

#[derive(Debug, Clone)]
pub struct DownloadOutcome {
    pub id: Uuid,
    pub status: String,
    pub external_id: Option<String>,
    pub error: Option<String>,
}

pub struct DownloadServiceContext<'a> {
    pub pool: &'a PgPool,
    pub http_client: &'a reqwest::Client,
    pub download_events: &'a broadcast::Sender<DownloadStreamEvent>,
    pub download_clients: &'a dyn DownloadClientResolver,
    pub indexers: &'a dyn IndexerResolver,
}

#[derive(Debug)]
pub struct DownloadServiceError {
    pub status: StatusCode,
    pub message: String,
}

impl DownloadServiceError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for DownloadServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for DownloadServiceError {}

/// How far back to look for a same-URL download when deduplicating automated
/// grabs. Wide enough to span kobbler's pattern teardown + re-`want` cycle (the
/// duplicate that imported, then got re-grabbed ~1.5h later), short enough not
/// to block a deliberate re-request days later.
const DUPLICATE_URL_WINDOW_HOURS: i32 = 48;

pub async fn submit_download(
    ctx: &DownloadServiceContext<'_>,
    input: DownloadSubmission,
) -> Result<DownloadOutcome, DownloadServiceError> {
    let protocol = input.protocol.to_lowercase();
    if protocol != "torrent" && protocol != "usenet" {
        return Err(DownloadServiceError::new(
            StatusCode::BAD_REQUEST,
            "protocol must be torrent or usenet",
        ));
    }

    // Look up source's debrid_mode if available (needed for client routing)
    let debrid_mode = if let Some(source_id) = input.indexer_source_id {
        watch_repo::get_indexer_source_by_id(ctx.pool, source_id)
            .await
            .ok()
            .flatten()
            .map(|s| s.debrid_mode)
    } else {
        None
    };

    let download_client = if let Some(client_id) = input.download_client_id {
        clients_repo::get_enabled_client_by_id(ctx.pool, client_id)
            .await
            .map_err(|e| {
                DownloadServiceError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
            })?
    } else {
        // Debrid-aware routing: if source is debrid-eligible, prefer debrid client.
        // Sources with debrid_mode 'always'/'both' route to debrid, as do
        // built-in hash index releases (no indexer_source_id, identified by source prefix).
        let is_hash_index_release = input.source.starts_with("hash_index:");
        let use_debrid = protocol == "torrent"
            && (matches!(debrid_mode.as_deref(), Some("always" | "both")) || is_hash_index_release);

        if use_debrid {
            // Try debrid client first, fall back to default torrent client
            let debrid = clients_repo::get_default_debrid_client(ctx.pool)
                .await
                .map_err(|e| {
                    DownloadServiceError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
                })?;
            if debrid.is_some() {
                debrid
            } else {
                clients_repo::get_default_client_for_protocol(ctx.pool, &protocol)
                    .await
                    .map_err(|e| {
                        DownloadServiceError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
                    })?
            }
        } else {
            clients_repo::get_default_client_for_protocol(ctx.pool, &protocol)
                .await
                .map_err(|e| {
                    DownloadServiceError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
                })?
        }
    }
    .ok_or_else(|| {
        DownloadServiceError::new(
            StatusCode::BAD_REQUEST,
            format!("no enabled download client for protocol {protocol}"),
        )
    })?;

    // Debrid client uses protocol "torrent" so this check still works
    if download_client.protocol != protocol {
        return Err(DownloadServiceError::new(
            StatusCode::BAD_REQUEST,
            "download client protocol mismatch",
        ));
    }

    let mut download_url =
        normalize_download_url(&input.download_url, input.url_context.base_url.as_deref())?;

    // dedup_key for automated grabs (watch/want). Derived from the pre-`prepare_download`
    // URL so it's stable across submissions for the same release (prepare may rewrite the
    // URL per-submission). NULL for manual/user downloads, which may legitimately repeat.
    let dedup_key = input
        .skip_duplicate_url
        .then(|| const_hex::encode(Sha256::digest(download_url.as_bytes())));

    // Fast-path: a recent active download already covers this URL. Catches the later
    // `want` re-grab of a release that already imported (kobbler tears the pattern down
    // post-import, so the watch_matches guard no longer sees it). The reservation below
    // catches the tighter ~1s double-fire that this read-check can race.
    if input.skip_duplicate_url
        && let Some(existing) = suppress_duplicate(ctx, &input, &download_url).await
    {
        return Ok(existing);
    }

    // Reserve the URL before any client submission. The partial unique index on dedup_key
    // rejects a concurrent in-flight submission for the same URL (returns None), so the
    // second of two racing watch patterns never reaches the download client. A NULL
    // dedup_key always inserts. We finalize this row's real status after the client add.
    let reservation = downloads_repo::DownloadInput {
        source: input.source.clone(),
        protocol: protocol.clone(),
        profile_id: None,
        name: input.name.clone(),
        download_url: download_url.clone(),
        download_client_id: download_client.id,
        external_id: None,
        callback_url: input.callback_url.clone(),
        auth_token: input.auth_token.clone(),
        metadata: Some(input.metadata.clone()),
        status: "submitting".to_string(),
        progress: None,
        error: None,
        release_payload: input.release_payload.clone(),
        api_key_id: input.api_key_id,
        quality_profile_id: input.quality_profile_id,
        media_kind: input.media_kind.clone(),
        submission_error: None,
    };
    let reserved_id = downloads_repo::reserve_download(ctx.pool, reservation, dedup_key.as_deref())
        .await
        .map_err(|e| DownloadServiceError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
    let reserved_id = match reserved_id {
        Some(id) => Some(id),
        // Reservation lost the race. The winner almost always still holds an active row —
        // return it as suppressed. If it has already been released (winner failed), fall
        // through with no reservation and create a fresh row after the client add.
        None => {
            if let Some(existing) = suppress_duplicate(ctx, &input, &download_url).await {
                return Ok(existing);
            }
            None
        }
    };

    // Call prepare_download on the indexer handler (e.g. MAM buys freeleech wedge)
    if let Some(source_id) = input.indexer_source_id
        && let Ok(Some(source)) = watch_repo::get_indexer_source_by_id(ctx.pool, source_id).await
        && let Some(handler) = ctx.indexers.get(&source.handler_type)
    {
        let idx_ctx = IndexerContext {
            pool: ctx.pool,
            http_client: ctx.http_client,
        };
        match handler
            .prepare_download(&idx_ctx, &source, &download_url)
            .await
        {
            Ok(new_url) => download_url = new_url,
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    handler = source.handler_type,
                    "prepare_download failed, continuing with original URL"
                );
            }
        }
    }

    // Apply per-indexer seeding overrides to the download client config
    let mut download_client = download_client;
    if let Some(ratio) = input.url_context.seed_ratio {
        download_client.seed_ratio = Some(ratio);
    }
    if let Some(time) = input.url_context.seed_time_minutes {
        download_client.seed_time_minutes = Some(time);
    }

    let (external_id, status, error, submission_error) = match fetch_release_payload(
        ctx.http_client,
        &download_url,
        &protocol,
        &input.url_context,
    )
    .await
    {
        Ok(release_payload) => {
            let handler = ctx
                .download_clients
                .get(download_client.client_type.as_str())
                .ok_or(DownloadClientError::UnsupportedClient);

            let add_result = match handler {
                Ok(handler) => match release_payload {
                    ReleasePayload::Bytes(bytes) => {
                        handler
                            .add_bytes(ctx.http_client, &download_client, &bytes, &input.name)
                            .await
                    }
                    ReleasePayload::Url(url) => {
                        handler
                            .add_url(ctx.http_client, &download_client, &url)
                            .await
                    }
                },
                Err(err) => Err(err),
            };

            match add_result {
                Ok(ext_id) => (Some(ext_id), "downloading".to_string(), None, None),
                Err(err) => {
                    // Transient errors (connection refused, timeout) -> pending_retry
                    // Permanent errors (auth failure, invalid URL) -> failed
                    if err.is_transient() {
                        (
                            None,
                            "pending_retry".to_string(),
                            None,
                            Some(err.to_string()),
                        )
                    } else {
                        (None, "failed".to_string(), Some(err.to_string()), None)
                    }
                }
            }
        }
        Err(err) => {
            // Fetch errors are transient if they're connection errors
            if err.is_transient() {
                (
                    None,
                    "pending_retry".to_string(),
                    None,
                    Some(err.to_string()),
                )
            } else {
                (None, "failed".to_string(), Some(err.to_string()), None)
            }
        }
    };

    let progress = if status == "failed" || status == "pending_retry" {
        None
    } else {
        Some(0.0)
    };

    // Finalize the reserved row, or (rare released-key fallback) create a fresh one.
    let id = if let Some(id) = reserved_id {
        downloads_repo::finalize_submission(
            ctx.pool,
            id,
            &status,
            external_id.as_deref(),
            error.as_deref(),
            submission_error.as_deref(),
            progress,
            &download_url,
        )
        .await
        .map_err(|e| DownloadServiceError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?;
        id
    } else {
        let input_row = downloads_repo::DownloadInput {
            source: input.source.clone(),
            protocol: protocol.clone(),
            profile_id: None,
            name: input.name.clone(),
            download_url: download_url.clone(),
            download_client_id: download_client.id,
            external_id: external_id.clone(),
            callback_url: input.callback_url.clone(),
            auth_token: input.auth_token.clone(),
            metadata: Some(input.metadata.clone()),
            status: status.clone(),
            progress,
            error: error.clone(),
            release_payload: input.release_payload.clone(),
            api_key_id: input.api_key_id,
            quality_profile_id: input.quality_profile_id,
            media_kind: input.media_kind.clone(),
            submission_error: submission_error.clone(),
        };
        downloads_repo::create_download(ctx.pool, input_row)
            .await
            .map_err(|e| {
                DownloadServiceError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
            })?
    };

    log_event(
        ctx.pool,
        id,
        "created",
        &format!("Download request received for '{}'", input.name),
        Some(serde_json::json!({
            "source": input.source,
            "protocol": protocol,
        })),
    )
    .await;

    if let Some(ref ext_id) = external_id {
        log_event(
            ctx.pool,
            id,
            "client_add_success",
            "Sent to download client successfully",
            Some(serde_json::json!({
                "client_name": download_client.name,
                "external_id": ext_id,
            })),
        )
        .await;
    } else if let Some(ref err) = submission_error {
        log_event(
            ctx.pool,
            id,
            "submission_pending_retry",
            "Download client temporarily unavailable, will retry",
            Some(serde_json::json!({ "error": err })),
        )
        .await;
    } else if let Some(ref err) = error {
        log_event(
            ctx.pool,
            id,
            "client_add_failed",
            "Failed to add to download client",
            Some(serde_json::json!({ "error": err })),
        )
        .await;
    }

    // Link out-of-band grabs (manual UI/API downloads) to the watch pattern they
    // satisfy. Without this the pattern's coverage checks never see the grab, and
    // the next backlog search re-downloads episodes a manual season pack already
    // filled. Best-effort: a failure here must not fail the download itself.
    record_out_of_band_match(ctx, &input, id, &download_url).await;

    broadcast_download_created(ctx.pool, ctx.download_events, id).await;

    info!(
        download_id = %id,
        status = %status,
        external_id = ?external_id,
        "Created download"
    );

    Ok(DownloadOutcome {
        id,
        status,
        external_id,
        error,
    })
}

/// Whether a release title's parsed season is the season this pattern wants.
///
/// Non-TvSeason patterns have no season to check and pass trivially. A title we
/// cannot parse a season out of fails: an episode key derived from it would be a
/// coverage claim we cannot back up.
fn release_season_matches_pattern(release_title: &str, media_want: &serde_json::Value) -> bool {
    if !crate::watch::episode_key::is_tv_season(media_want) {
        return true;
    }
    let Some(wanted) = media_want
        .get("season")
        .and_then(|v| v.as_str())
        .and_then(|s| s.parse::<u32>().ok())
    else {
        return false;
    };
    let crate::parsing::ParsedMedia::Tv(tv) =
        crate::parsing::parse_release(release_title, "tv").media
    else {
        return false;
    };
    tv.season == Some(wanted)
}

/// Record a `watch_matches` row for a download that did not come from the watch
/// pipeline (manual UI/API grabs), linking it to the pattern it satisfies.
///
/// Every coverage check in Kobload — `episode_has_active_download`,
/// `episode_has_imported_download`, the backlog-search early exit — keys off
/// `watch_matches`. A grab submitted straight to `POST /downloads` had no such
/// row, so a manually-grabbed season pack was invisible: the next backlog search
/// saw an uncovered season and re-downloaded it episode by episode.
///
/// The pattern is located by trying each of Kobbler's id fields against
/// `watch_patterns.source_id` — which one is authoritative varies by want kind
/// (see below). Best-effort throughout: no pattern, unparsable title, or a DB
/// error simply means no row, never a failed download — the download itself has
/// already succeeded by the time this runs.
async fn record_out_of_band_match(
    ctx: &DownloadServiceContext<'_>,
    input: &DownloadSubmission,
    download_id: Uuid,
    download_url: &str,
) {
    // Only the manual routes reach here. The want and watch pipelines set
    // `skip_duplicate_url` and write their own match row — a reservation created
    // BEFORE this call and linked to the download after it, so checking
    // `watch_matches.download_id` here would not see it and we would insert a
    // duplicate keyed on the URL rather than the indexer GUID.
    if input.skip_duplicate_url {
        return;
    }

    // A manual grab carries Kobbler's want fields at the top level; tolerate the
    // nested `original_metadata` shape too, since callers may forward either.
    let meta = input
        .metadata
        .get("original_metadata")
        .filter(|v| v.is_object())
        .unwrap_or(&input.metadata);

    // Which metadata field is the pattern's `source_id` depends on the want kind:
    // TV seasons register under `season_id` (tv_want), sport events under the
    // EVENT id (sports_want) even though the grab metadata also carries a
    // `season_id`, and movies under `movie_id`. Kobbler sends several of these at
    // once, so try each and take the one that actually resolves rather than
    // guessing from `source_type`.
    let mut pattern = None;
    for key in ["season_id", "episode_id", "movie_id"] {
        let Some(candidate) = meta.get(key).and_then(|v| v.as_str()) else {
            continue;
        };
        match watch_repo::get_pattern_by_source(ctx.pool, "kobbler", candidate).await {
            Ok(Some(p)) => {
                pattern = Some(p);
                break;
            }
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(
                    error = %e,
                    source_id = candidate,
                    "Failed to look up watch pattern for out-of-band download"
                );
                return;
            }
        }
    }
    let Some(pattern) = pattern else {
        return;
    };

    // TvSeason patterns track coverage per episode; everything else is
    // pattern-level and takes a NULL episode_key.
    //
    // The season is verified against the pattern first. `extract_episode_key`
    // parses the title in isolation and happily returns a bare `SEASON_PACK` for
    // a DIFFERENT season's pack (and for a multi-season "S01-S05" title, keeping
    // only the first season) — the automated pipeline never sees those because
    // scoring rejects them upstream, but a manual grab has no such filter. A
    // wrong `SEASON_PACK` row would mark this whole season covered and suppress
    // every future search for it, so on any mismatch we record a pattern-level
    // row (NULL episode_key) rather than a coverage claim we cannot justify.
    let episode_key = pattern
        .media_want
        .as_ref()
        .filter(|mw| release_season_matches_pattern(&input.name, mw))
        .and_then(|mw| crate::watch::episode_key::extract_episode_key(&input.name, mw));

    let release_data = serde_json::json!({
        "title": input.name,
        "download_url": download_url,
        "protocol": input.protocol,
        "source": input.source,
        "manual": true,
    });

    let match_input = watch_repo::WatchMatchInput {
        pattern_id: pattern.id,
        indexer_source_id: input.indexer_source_id,
        // Manual grabs have no indexer GUID; the URL is the stable identity and
        // keeps the (pattern_id, release_guid) unique index meaningful.
        release_guid: download_url,
        release_title: &input.name,
        release_data: Some(&release_data),
        match_score: 1.0,
        matched_pattern: "manual",
        action: "downloaded",
        download_id: Some(download_id),
        hold_until: None,
        held: false,
        quality_score: None,
        rejected: false,
        rejected_at: None,
        rejected_reason: None,
        episode_key: episode_key.as_deref(),
    };

    match watch_repo::create_match_if_not_exists(ctx.pool, &match_input).await {
        Ok(Some(_)) => info!(
            download_id = %download_id,
            pattern_id = %pattern.id,
            episode_key = ?episode_key,
            "Linked out-of-band download to watch pattern"
        ),
        Ok(None) => {}
        Err(e) => tracing::warn!(
            error = %e,
            download_id = %download_id,
            pattern_id = %pattern.id,
            "Failed to record watch match for out-of-band download"
        ),
    }
}

/// If a recent active download already covers `download_url`, log it and return a
/// suppressed outcome pointing at that existing download. Used both by the pre-reserve
/// read-check and the post-reserve race-loss path.
async fn suppress_duplicate(
    ctx: &DownloadServiceContext<'_>,
    input: &DownloadSubmission,
    download_url: &str,
) -> Option<DownloadOutcome> {
    let existing = downloads_repo::find_recent_active_download_by_url(
        ctx.pool,
        download_url,
        DUPLICATE_URL_WINDOW_HOURS,
    )
    .await
    .ok()
    .flatten()?;

    info!(
        existing_download_id = %existing.id,
        existing_status = %existing.status,
        source = %input.source,
        name = %input.name,
        "Suppressing duplicate download: active download already exists for this URL"
    );
    log_event(
        ctx.pool,
        existing.id,
        "duplicate_suppressed",
        &format!(
            "Suppressed duplicate submission for '{}' (source {})",
            input.name, input.source
        ),
        Some(serde_json::json!({
            "source": input.source,
            "existing_status": existing.status,
        })),
    )
    .await;
    Some(DownloadOutcome {
        id: existing.id,
        status: existing.status,
        external_id: existing.external_id,
        error: None,
    })
}

fn normalize_download_url(
    download_url: &str,
    base_url: Option<&str>,
) -> Result<String, DownloadServiceError> {
    if download_url.starts_with('/') {
        let base_url = base_url.ok_or_else(|| {
            DownloadServiceError::new(
                StatusCode::BAD_REQUEST,
                "download_url is relative but base_url is missing",
            )
        })?;
        let base = base_url.trim_end_matches('/');
        return Ok(format!("{base}{download_url}"));
    }
    Ok(download_url.to_string())
}

fn is_magnet_url(value: &str) -> bool {
    value.trim_start().starts_with("magnet:")
}

enum ReleasePayload {
    Bytes(Vec<u8>),
    Url(String),
}

async fn fetch_release_payload(
    http_client: &reqwest::Client,
    download_url: &str,
    protocol: &str,
    url_context: &DownloadUrlContext,
) -> Result<ReleasePayload, DownloadClientError> {
    if is_magnet_url(download_url) {
        if protocol != "torrent" {
            return Err(DownloadClientError::Message(
                "magnet URLs are only supported for torrent downloads".to_string(),
            ));
        }
        return Ok(ReleasePayload::Url(download_url.to_string()));
    }

    let mut request = http_client
        .get(download_url)
        .header("User-Agent", "Kobload/1.0");
    if let Some((key, value)) = &url_context.auth_header {
        request = request.header(key, value);
    }
    if let Some(ref cookie) = url_context.cookie {
        request = request.header("Cookie", cookie);
    }

    let resp = request.send().await.map_err(DownloadClientError::Request)?;

    if !resp.status().is_success() {
        return Err(DownloadClientError::Status(resp.status()));
    }

    resp.bytes()
        .await
        .map(|b| ReleasePayload::Bytes(b.to_vec()))
        .map_err(DownloadClientError::Request)
}

/// Retry submitting a download to a download client.
/// Used by the polling loop for downloads in pending_retry status.
pub async fn retry_submission(
    http_client: &reqwest::Client,
    download: &downloads_repo::PendingRetryDownload,
    client: &crate::types::DownloadClientConfig,
    handler: &dyn crate::download_clients::DownloadClientHandler,
) -> Result<String, DownloadClientError> {
    // For magnet URLs, just add directly
    if is_magnet_url(&download.download_url) {
        return handler
            .add_url(http_client, client, &download.download_url)
            .await;
    }

    // Fetch the payload (torrent file, NZB, etc.)
    let request = http_client
        .get(&download.download_url)
        .header("User-Agent", "Kobload/1.0");

    let resp = request.send().await.map_err(DownloadClientError::Request)?;

    if !resp.status().is_success() {
        return Err(DownloadClientError::Status(resp.status()));
    }

    let bytes = resp
        .bytes()
        .await
        .map_err(DownloadClientError::Request)?
        .to_vec();

    handler
        .add_bytes(http_client, client, &bytes, &download.name)
        .await
}
