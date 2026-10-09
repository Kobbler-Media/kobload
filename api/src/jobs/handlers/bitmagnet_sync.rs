//! Scheduled job that syncs torrents from BitMagnet's GraphQL API
//! into the local hash index.
//!
//! Paginates through BitMagnet's torrent content, ordered by `updated_at`,
//! tracking a cursor in `app_settings` for incremental sync.

use std::time::Instant;

use async_trait::async_trait;
use chrono::{DateTime, Duration as ChronoDuration, Utc};
use serde::Deserialize;
use serde_json::Value;
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};
use uuid::Uuid;

use crate::jobs::JobError;
use crate::jobs::worker::JobHandler;
use crate::parsing::{ParsedMedia, guess_media_kind, parse_release};
use crate::repositories::app_settings;
use crate::repositories::hash_index::{self, HashFileEntry, HashIndexEntry};

const DEFAULT_BITMAGNET_URL: &str = "http://bitmagnet:3333";
const PAGE_SIZE: i64 = 1000;
const CURSOR_KEY: &str = "bitmagnet_sync_cursor";
const REQUEST_TIMEOUT_SECS: u64 = 60;

/// Handler for bitmagnet_sync jobs.
pub struct BitmagnetSyncHandler {
    pool: PgPool,
    http_client: reqwest::Client,
}

impl BitmagnetSyncHandler {
    pub fn new(pool: PgPool, http_client: reqwest::Client) -> Self {
        Self { pool, http_client }
    }
}

enum FetchError {
    Unreachable,
    Failed(String),
}

impl BitmagnetSyncHandler {
    /// Fetch a single page of torrent content from BitMagnet's GraphQL API.
    async fn fetch_page(
        &self,
        graphql_url: &str,
        limit: i64,
        offset: i64,
        descending: bool,
    ) -> Result<(Vec<BitmagnetItem>, Option<i64>, Option<bool>), FetchError> {
        let query_str = build_graphql_query(descending);

        let body = serde_json::json!({
            "query": query_str,
            "variables": { "limit": limit, "offset": offset }
        });

        let response = match self
            .http_client
            .post(graphql_url)
            .json(&body)
            .timeout(std::time::Duration::from_secs(REQUEST_TIMEOUT_SECS))
            .send()
            .await
        {
            Ok(resp) => resp,
            Err(e) if e.is_connect() || e.is_timeout() => {
                return Err(FetchError::Unreachable);
            }
            Err(e) => {
                return Err(FetchError::Failed(format!("BitMagnet request failed: {e}")));
            }
        };

        if !response.status().is_success() {
            let status = response.status();
            let body_text = response.text().await.unwrap_or_default();
            return Err(FetchError::Failed(format!(
                "BitMagnet returned {status}: {body_text}"
            )));
        }

        let parsed: BitmagnetResponse = response
            .json()
            .await
            .map_err(|e| FetchError::Failed(format!("Failed to parse BitMagnet response: {e}")))?;

        let search = parsed
            .data
            .and_then(|d| d.torrent_content)
            .and_then(|tc| tc.search);

        match search {
            Some(s) => Ok((s.items, s.total_count, s.has_next_page)),
            None => Ok((vec![], None, None)),
        }
    }

    /// Convert BitMagnet items to HashIndexEntry with quality parsing.
    /// Returns (entries, info_hashes_with_files) where the second vec contains
    /// hashes that have file data available in BitMagnet for a separate fetch.
    fn convert_items(&self, items: Vec<BitmagnetItem>) -> (Vec<HashIndexEntry>, Vec<String>) {
        let mut entries = Vec::with_capacity(items.len());
        let mut hashes_with_files = Vec::new();

        for item in items {
            if item.info_hash.is_empty() {
                continue;
            }

            // Extract IMDB/TMDB IDs from content attributes or content source
            let (mut imdb_id, mut tmdb_id) = (None::<String>, None::<String>);
            if let Some(ref content) = item.content {
                // Check content.source + content.id (primary metadata source)
                if let (Some(src), Some(id)) = (&content.source, &content.id) {
                    match src.as_str() {
                        "imdb" => imdb_id = Some(id.clone()),
                        "tmdb" => tmdb_id = Some(id.clone()),
                        _ => {}
                    }
                }
                // Check attributes for additional IDs
                if let Some(ref attrs) = content.attributes {
                    for attr in attrs {
                        match attr.source.as_str() {
                            "imdb" if imdb_id.is_none() => {
                                imdb_id = Some(attr.value.clone());
                            }
                            "tmdb" if tmdb_id.is_none() => {
                                tmdb_id = Some(attr.value.clone());
                            }
                            _ => {}
                        }
                    }
                }
            }

            let content_type = item.content.as_ref().and_then(|c| c.r#type.as_deref());

            let size = item.torrent.as_ref().and_then(|t| t.size);
            let seeders = item
                .seeders
                .or_else(|| item.torrent.as_ref().and_then(|t| t.seeders));
            let magnet_url = item.torrent.as_ref().and_then(|t| t.magnet_uri.clone());
            let info_hash = item.info_hash.to_lowercase();

            // Track hashes that have file data available for separate fetch
            if let Some(ref torrent) = item.torrent
                && torrent.has_files_info == Some(true)
            {
                hashes_with_files.push(info_hash.clone());
            }

            // Prefer original torrent name (release name) over BitMagnet's cleaned title
            let title = item
                .torrent
                .as_ref()
                .and_then(|t| t.name.clone())
                .filter(|n| !n.is_empty())
                .or(item.title)
                .unwrap_or_else(|| info_hash.clone());

            // Use BitMagnet's content type if available, fall back to title heuristic
            let media_kind = content_type_to_media_kind(content_type, &title);
            let parsed = parse_release(&title, media_kind);
            let quality_json = serde_json::to_value(&parsed.quality).ok();
            let year = match &parsed.media {
                ParsedMedia::Movie(m) => m.year.and_then(|y| i16::try_from(y).ok()),
                _ => None,
            };

            // Always re-parse as TV for S/E — `parsed` above may have been
            // classified as "movies" by content_type_to_media_kind when
            // content_type is missing, which would skip S/E detection on
            // space-separated TV titles.
            let (season, episodes) =
                crate::repositories::hash_index::derive_season_episodes(&title);

            entries.push(HashIndexEntry {
                info_hash,
                imdb_id,
                tmdb_id,
                original_title: title,
                parsed_title: Some(parsed.title),
                year,
                quality: quality_json,
                size,
                magnet_url,
                source: "dht".to_string(),
                seeders,
                season,
                episodes,
            });
        }

        (entries, hashes_with_files)
    }
}

#[async_trait]
impl JobHandler for BitmagnetSyncHandler {
    fn job_type(&self) -> &'static str {
        "bitmagnet_sync"
    }

    fn max_concurrent(&self) -> usize {
        1
    }

    async fn execute(
        &self,
        _job_id: Uuid,
        _payload: Value,
        cancel: CancellationToken,
    ) -> Result<Value, JobError> {
        nats_jobs::check_cancelled(&cancel)?;
        let start = Instant::now();

        // Resolve BitMagnet URL
        let bitmagnet_url = app_settings::get(&self.pool, "bitmagnet_url")
            .await
            .ok()
            .flatten()
            .and_then(|v| v.as_str().map(String::from))
            .unwrap_or_else(|| DEFAULT_BITMAGNET_URL.to_string());
        let graphql_url = format!("{bitmagnet_url}/graphql");

        // Load cursor from previous run
        let stored_cursor = app_settings::get(&self.pool, CURSOR_KEY)
            .await
            .ok()
            .flatten()
            .and_then(|v| v.as_str().map(String::from));

        let is_initial = stored_cursor.is_none();

        if is_initial {
            info!(
                url = %graphql_url,
                "bitmagnet sync: initial full sync — no cursor found, will paginate all items (ascending)"
            );
        } else {
            info!(
                url = %graphql_url,
                cursor = ?stored_cursor,
                "bitmagnet sync: incremental sync — fetching newest items first (descending)"
            );
        }

        // For initial sync: ascending order to paginate through everything.
        // For incremental: descending order (newest first) so we can stop early
        // once we hit items older than our cursor.
        let descending = !is_initial;

        let cursor_ts = stored_cursor.as_ref().and_then(|s| {
            DateTime::parse_from_rfc3339(s)
                .ok()
                .map(|dt| dt.with_timezone(&Utc))
        });

        let mut offset: i64 = 0;
        let mut total_synced: u64 = 0;
        let mut max_updated_at: Option<DateTime<Utc>> = cursor_ts;
        let mut page_num: u64 = 0;
        let mut hit_cursor = false;

        loop {
            nats_jobs::check_cancelled(&cancel)?;

            let (items, total_count, has_next_page) = match self
                .fetch_page(&graphql_url, PAGE_SIZE, offset, descending)
                .await
            {
                Ok(result) => result,
                Err(FetchError::Unreachable) => {
                    warn!(
                        url = %graphql_url,
                        pages_completed = page_num,
                        synced = total_synced,
                        "bitmagnet sync: service not reachable, stopping"
                    );
                    // Save progress if we synced anything before the connection dropped
                    if total_synced > 0 {
                        self.save_cursor(&max_updated_at).await;
                    }
                    return Ok(serde_json::json!({
                        "synced": total_synced,
                        "pages": page_num,
                        "skipped": "bitmagnet not reachable"
                    }));
                }
                Err(FetchError::Failed(msg)) => {
                    // Save progress from successfully synced pages
                    if total_synced > 0 {
                        self.save_cursor(&max_updated_at).await;
                        warn!(
                            synced = total_synced,
                            pages = page_num,
                            "bitmagnet sync: error after partial sync, progress saved — {msg}"
                        );
                    }
                    return Err(JobError::ExecutionFailed(msg));
                }
            };

            if items.is_empty() {
                info!(
                    pages = page_num,
                    total_synced, "bitmagnet sync: no more items, pagination complete"
                );
                break;
            }

            page_num += 1;
            let page_item_count = items.len() as i64;

            // Track max updatedAt and check if we've reached our cursor
            let mut page_older_count = 0i64;
            for item in &items {
                if let Some(ref ts_str) = item.updated_at
                    && let Ok(ts) = DateTime::parse_from_rfc3339(ts_str)
                {
                    let ts_utc = ts.with_timezone(&Utc);
                    if max_updated_at.is_none_or(|cur| ts_utc > cur) {
                        max_updated_at = Some(ts_utc);
                    }
                    if descending
                        && let Some(cursor) = cursor_ts
                        && ts_utc <= cursor
                    {
                        page_older_count += 1;
                    }
                }
            }

            // Convert and upsert
            let (entries, hashes_with_files) = self.convert_items(items);
            let count = entries.len() as u64;

            hash_index::upsert_batch(&self.pool, &entries)
                .await
                .map_err(|e| {
                    JobError::ExecutionFailed(format!("Failed to upsert BitMagnet entries: {e}"))
                })?;

            // Fetch file data via separate torrent.files query (search doesn't resolve files)
            if !hashes_with_files.is_empty() {
                match self.fetch_files(&graphql_url, &hashes_with_files).await {
                    Ok(files) if !files.is_empty() => {
                        if let Err(e) = hash_index::upsert_files(&self.pool, &files).await {
                            warn!(error = %e, "bitmagnet sync: failed to upsert file entries");
                        }
                    }
                    Err(e) => {
                        warn!(error = %e, "bitmagnet sync: failed to fetch file data");
                    }
                    _ => {}
                }
            }

            total_synced += count;
            offset += PAGE_SIZE;

            info!(
                page = page_num,
                page_items = page_item_count,
                total_synced,
                total_available = total_count.unwrap_or(-1),
                "bitmagnet sync: page complete"
            );

            // In descending mode, stop if most items on this page are older than cursor
            // (we've caught up with what we already have)
            if descending && page_older_count > page_item_count / 2 {
                info!(
                    older_items = page_older_count,
                    page_items = page_item_count,
                    "bitmagnet sync: reached previously synced items, stopping"
                );
                hit_cursor = true;
                break;
            }

            // Stop if no more pages
            if has_next_page == Some(false) {
                break;
            }
            if page_item_count < PAGE_SIZE {
                break;
            }
            if let Some(tc) = total_count
                && offset >= tc
            {
                break;
            }
        }

        // Persist cursor
        self.save_cursor(&max_updated_at).await;

        let elapsed = start.elapsed();
        info!(
            total_synced,
            pages = page_num,
            elapsed_ms = elapsed.as_millis(),
            is_initial,
            hit_cursor,
            new_cursor = ?max_updated_at.map(|t| t.to_rfc3339()),
            "bitmagnet sync: complete"
        );

        Ok(serde_json::json!({
            "synced": total_synced,
            "pages": page_num,
            "elapsed_ms": elapsed.as_millis() as u64,
            "initial": is_initial,
        }))
    }
}

impl BitmagnetSyncHandler {
    async fn save_cursor(&self, max_updated_at: &Option<DateTime<Utc>>) {
        if let Some(ts) = max_updated_at {
            // Subtract a small buffer to handle clock skew and items arriving during sync
            let safe_cursor = *ts - ChronoDuration::minutes(2);
            let cursor_str = safe_cursor.to_rfc3339();
            if let Err(e) = app_settings::set(
                &self.pool,
                CURSOR_KEY,
                &serde_json::Value::String(cursor_str.clone()),
            )
            .await
            {
                warn!(error = %e, "bitmagnet sync: failed to save cursor");
            }
        }
    }
}

const FILES_BATCH_SIZE: usize = 100;
const FILES_PAGE_LIMIT: i64 = 1000;

impl BitmagnetSyncHandler {
    /// Fetch file data for a set of info hashes via BitMagnet's `torrent.files` query.
    /// The `torrentContent.search` path never resolves files, so this separate query is needed.
    async fn fetch_files(
        &self,
        graphql_url: &str,
        info_hashes: &[String],
    ) -> std::result::Result<Vec<HashFileEntry>, String> {
        let mut all_files = Vec::new();

        for batch in info_hashes.chunks(FILES_BATCH_SIZE) {
            let hashes: Vec<&str> = batch.iter().map(|s| s.as_str()).collect();
            let mut page = 1i64;

            loop {
                let query = format!(
                    r#"query {{
                        torrent {{
                            files(input: {{
                                infoHashes: [{hash_list}],
                                limit: {limit},
                                page: {page},
                                totalCount: true,
                                hasNextPage: true
                            }}) {{
                                items {{ infoHash index path size }}
                                totalCount
                                hasNextPage
                            }}
                        }}
                    }}"#,
                    hash_list = hashes
                        .iter()
                        .map(|h| format!("\"{h}\""))
                        .collect::<Vec<_>>()
                        .join(", "),
                    limit = FILES_PAGE_LIMIT,
                    page = page,
                );

                let body = serde_json::json!({ "query": query });

                let response = self
                    .http_client
                    .post(graphql_url)
                    .json(&body)
                    .timeout(std::time::Duration::from_secs(REQUEST_TIMEOUT_SECS))
                    .send()
                    .await
                    .map_err(|e| format!("torrent.files request failed: {e}"))?;

                if !response.status().is_success() {
                    let status = response.status();
                    let body_text = response.text().await.unwrap_or_default();
                    return Err(format!("torrent.files returned {status}: {body_text}"));
                }

                let parsed: TorrentFilesResponse = response
                    .json()
                    .await
                    .map_err(|e| format!("Failed to parse torrent.files response: {e}"))?;

                let result = parsed.data.and_then(|d| d.torrent).and_then(|t| t.files);

                let Some(result) = result else {
                    break;
                };

                for file_item in &result.items {
                    let (season, episodes, media_type) =
                        hash_index::classify_hash_file(&file_item.path);
                    all_files.push(HashFileEntry {
                        info_hash: file_item.info_hash.to_lowercase(),
                        file_idx: file_item.index,
                        filename: file_item.path.clone(),
                        size: Some(file_item.size),
                        season,
                        episodes,
                        media_type,
                    });
                }

                if result.has_next_page != Some(true) {
                    break;
                }
                page += 1;
            }
        }

        Ok(all_files)
    }
}

/// Map BitMagnet's content type to a media kind for `parse_release`.
fn content_type_to_media_kind(content_type: Option<&str>, title: &str) -> &'static str {
    match content_type {
        Some("movie") => "movies",
        Some("tv_show" | "tvShow") => "tv",
        Some("music") => "music",
        Some("game" | "games") => "games",
        Some("software") => "software",
        Some("book" | "ebook" | "audiobook" | "comic") => "ebooks",
        Some("xxx") => "xxx",
        _ => guess_media_kind(title),
    }
}

fn build_graphql_query(descending: bool) -> String {
    let desc = if descending { "true" } else { "false" };
    format!(
        r#"
        query SyncTorrents($limit: Int!, $offset: Int!) {{
            torrentContent {{
                search(
                    input: {{
                        limit: $limit
                        offset: $offset
                        totalCount: true
                        hasNextPage: true
                        orderBy: [{{ field: updated_at, descending: {desc} }}]
                        facets: {{
                            contentType: {{ filter: [movie, tv_show, audiobook, ebook, comic] }}
                        }}
                    }}
                ) {{
                    items {{
                        infoHash
                        title
                        updatedAt
                        seeders
                        leechers
                        videoResolution
                        videoCodec
                        videoSource
                        releaseGroup
                        torrent {{
                            name
                            size
                            seeders
                            magnetUri
                            hasFilesInfo
                            filesCount
                        }}
                        content {{
                            type
                            source
                            id
                            attributes {{ source key value }}
                        }}
                    }}
                    totalCount
                    hasNextPage
                }}
            }}
        }}
        "#
    )
}

// ---------------------------------------------------------------------------
// BitMagnet GraphQL response types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct BitmagnetResponse {
    data: Option<BitmagnetData>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BitmagnetData {
    torrent_content: Option<BitmagnetTorrentContent>,
}

#[derive(Debug, Deserialize)]
struct BitmagnetTorrentContent {
    search: Option<BitmagnetSearchResult>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BitmagnetSearchResult {
    items: Vec<BitmagnetItem>,
    total_count: Option<i64>,
    has_next_page: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BitmagnetItem {
    info_hash: String,
    title: Option<String>,
    updated_at: Option<String>,
    seeders: Option<i32>,
    torrent: Option<BitmagnetTorrent>,
    content: Option<BitmagnetContent>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BitmagnetTorrent {
    name: Option<String>,
    size: Option<i64>,
    seeders: Option<i32>,
    magnet_uri: Option<String>,
    has_files_info: Option<bool>,
    #[allow(dead_code)]
    files_count: Option<i32>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BitmagnetContent {
    r#type: Option<String>,
    source: Option<String>,
    id: Option<String>,
    attributes: Option<Vec<BitmagnetAttribute>>,
}

#[derive(Debug, Deserialize)]
struct BitmagnetAttribute {
    source: String,
    #[allow(dead_code)]
    key: String,
    value: String,
}

// ---------------------------------------------------------------------------
// BitMagnet torrent.files response types (separate query path for file data)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct TorrentFilesResponse {
    data: Option<TorrentFilesData>,
}

#[derive(Debug, Deserialize)]
struct TorrentFilesData {
    torrent: Option<TorrentFilesQuery>,
}

#[derive(Debug, Deserialize)]
struct TorrentFilesQuery {
    files: Option<TorrentFilesResult>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TorrentFilesResult {
    items: Vec<TorrentFileItem>,
    #[allow(dead_code)]
    total_count: Option<i64>,
    has_next_page: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct TorrentFileItem {
    info_hash: String,
    index: i32,
    path: String,
    size: i64,
}
