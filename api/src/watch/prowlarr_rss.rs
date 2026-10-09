//! Prowlarr RSS-style handler for fetching recent releases.
//!
//! Uses Prowlarr's native API to get recent releases from configured indexers.
//! Deduplication is handled per-pattern by `watch_matches` table, not here.
//!
//! Watermarking: Tracks the newest seen GUID and publish_date to detect gaps.
//! Per-sub-indexer retry-after is tracked in watermark state (one source has
//! many Prowlarr indexer IDs). Source-level 429 is recorded in DB via
//! `record_rate_limited()` when all sub-indexers are rate-limited.

use std::collections::HashMap;

use serde::Deserialize;
use time::OffsetDateTime;
use tracing::{debug, error, info, warn};

use super::types::{IndexerSource, Release, RssCheckResult, RssWatermark};
use crate::prowlarr::{ProwlarrClient, ProwlarrError, ProwlarrRelease};
use crate::repositories::watch as watch_repo;

/// Handler config for prowlarr_rss
#[derive(Debug, Deserialize)]
pub struct ProwlarrRssConfig {
    #[serde(alias = "prowlarr_base_url")]
    pub base_url: String,
    #[serde(alias = "prowlarr_api_key")]
    pub api_key: String,
    /// Single indexer ID (legacy/convenience). Merged into `indexer_ids` at runtime.
    #[serde(default, alias = "prowlarr_indexer_id")]
    pub indexer_id: Option<i32>,
    #[serde(default)]
    pub indexer_ids: Vec<i32>,
    #[serde(default = "default_poll_interval")]
    #[allow(dead_code)]
    pub poll_interval_minutes: i32,
}

fn default_poll_interval() -> i32 {
    15
}

/// Check a Prowlarr RSS-style source for new releases.
///
/// Returns ALL fetched releases — per-pattern deduplication is handled
/// by `already_matched()` in the watch service using the `watch_matches` table.
///
/// Also returns watermark info for gap detection and state persistence.
pub async fn check_prowlarr_rss(
    pool: &sqlx::PgPool,
    source: &IndexerSource,
    http_client: &reqwest::Client,
) -> Result<Vec<Release>, Box<dyn std::error::Error + Send + Sync>> {
    let result = check_prowlarr_rss_with_watermark(pool, source, http_client).await?;
    Ok(result.releases)
}

/// Check Prowlarr RSS with watermark tracking for gap detection.
pub async fn check_prowlarr_rss_with_watermark(
    pool: &sqlx::PgPool,
    source: &IndexerSource,
    http_client: &reqwest::Client,
) -> Result<RssCheckResult, Box<dyn std::error::Error + Send + Sync>> {
    let config: ProwlarrRssConfig = serde_json::from_value(source.handler_config.clone())
        .map_err(|e| format!("Invalid prowlarr_rss config: {}", e))?;

    // Load existing watermark from source state
    let watermark: RssWatermark = source
        .state
        .as_ref()
        .and_then(|s| serde_json::from_value(s.clone()).ok())
        .unwrap_or_default();

    info!(
        source_id = %source.id,
        source_name = %source.name,
        indexer_ids = ?config.indexer_ids,
        last_seen_guid = ?watermark.last_seen_guid,
        "Checking Prowlarr for new releases"
    );

    let client = ProwlarrClient::new_with_client(
        config.base_url.clone(),
        config.api_key.clone(),
        http_client.clone(),
    );

    // Merge single indexer_id into indexer_ids
    let mut indexer_ids = config.indexer_ids.clone();
    if let Some(id) = config.indexer_id
        && !indexer_ids.contains(&id)
    {
        indexer_ids.push(id);
    }

    let categories = source.category_ids.clone().unwrap_or_default();
    let mut retry_after_map = watermark.indexer_retry_after.clone();
    let fetch_result = fetch_recent_releases(
        pool,
        source.id,
        &client,
        &indexer_ids,
        &categories,
        &watermark,
        &mut retry_after_map,
    )
    .await?;

    info!(
        source_id = %source.id,
        total_releases = fetch_result.releases.len(),
        "Fetched releases from Prowlarr"
    );

    // If watermark was proactively advanced due to long rate limits on all
    // sub-indexers, return with advanced watermark.
    if fetch_result.watermark_advanced {
        let now_rfc2822 = OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc2822)
            .unwrap_or_default();
        let new_watermark = Some(RssWatermark {
            last_seen_guid: None,
            last_seen_publish_date: Some(now_rfc2822),
            indexer_retry_after: retry_after_map,
        });
        return Ok(RssCheckResult {
            releases: fetch_result
                .releases
                .iter()
                .map(|r| prowlarr_to_release(r, &source.name, source.id))
                .collect(),
            new_watermark,
            gap_detected: true,
        });
    }

    if fetch_result.releases.is_empty() {
        // Still persist updated retry-after state even with no releases
        let new_watermark = if retry_after_map != watermark.indexer_retry_after {
            Some(RssWatermark {
                indexer_retry_after: retry_after_map,
                ..watermark
            })
        } else {
            None
        };
        return Ok(RssCheckResult {
            releases: Vec::new(),
            new_watermark,
            gap_detected: false,
        });
    }

    let prowlarr_releases = &fetch_result.releases;

    // Find the chronologically newest and oldest across all indexers
    let newest = prowlarr_releases
        .iter()
        .max_by(|a, b| cmp_publish_dates(&a.publish_date, &b.publish_date))
        .unwrap(); // safe: we checked non-empty above
    let oldest = prowlarr_releases
        .iter()
        .min_by(|a, b| cmp_publish_dates(&a.publish_date, &b.publish_date))
        .unwrap();

    // Detect gap: if we have a watermark and the oldest returned item is newer than it
    let gap_detected = if let Some(ref last_date) = watermark.last_seen_publish_date {
        date_is_newer(&oldest.publish_date, last_date)
    } else {
        false
    };

    if gap_detected {
        warn!(
            source_id = %source.id,
            source_name = %source.name,
            oldest_in_batch = %oldest.publish_date,
            last_seen = ?watermark.last_seen_publish_date,
            "Gap detected in RSS feed - some releases may have been missed. Backlog search will catch up."
        );
    }

    // Create new watermark from the newest release, preserving retry-after state
    let new_watermark = Some(RssWatermark {
        last_seen_guid: Some(newest.guid.clone()),
        last_seen_publish_date: Some(newest.publish_date.clone()),
        indexer_retry_after: retry_after_map,
    });

    let releases: Vec<Release> = prowlarr_releases
        .iter()
        .map(|r| prowlarr_to_release(r, &source.name, source.id))
        .collect();

    Ok(RssCheckResult {
        releases,
        new_watermark,
        gap_detected,
    })
}

/// Soft catchup cap — only stops pagination when combined with result threshold.
const MAX_CATCHUP_SECONDS: i64 = 24 * 3600;

/// Absolute maximum catchup regardless of result count (7 days).
const HARD_CATCHUP_SECONDS: i64 = 7 * 24 * 3600;

/// Result count threshold for the soft catchup cap (matching Sonarr).
const CATCHUP_RESULT_THRESHOLD: usize = 1000;

/// Maximum Retry-After we'll honour (12 hours).
const MAX_RETRY_AFTER_SECONDS: u64 = 12 * 3600;

/// Retry-after threshold above which we proactively advance the watermark.
const PROACTIVE_ADVANCE_THRESHOLD_SECS: u64 = 6 * 3600;

/// Delay between pagination requests to avoid bursting rate limits.
const PAGINATION_DELAY: std::time::Duration = std::time::Duration::from_secs(2);

/// Try to parse an RSS publish date (RFC 2822) into an OffsetDateTime.
fn parse_publish_date(date_str: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(date_str, &time::format_description::well_known::Rfc2822).ok()
}

/// Compare two publish date strings chronologically.
fn cmp_publish_dates(a: &str, b: &str) -> std::cmp::Ordering {
    if let (Some(a_dt), Some(b_dt)) = (parse_publish_date(a), parse_publish_date(b)) {
        a_dt.cmp(&b_dt)
    } else {
        a.cmp(b)
    }
}

/// Compare two publish date strings — true if `a` is strictly newer than `b`.
fn date_is_newer(a: &str, b: &str) -> bool {
    cmp_publish_dates(a, b) == std::cmp::Ordering::Greater
}

/// Compare two publish date strings — true if `a` is at or before `b`.
fn date_is_at_or_before(a: &str, b: &str) -> bool {
    !date_is_newer(a, b)
}

/// Result from `fetch_recent_releases` including rate-limit metadata.
struct FetchResult {
    releases: Vec<ProwlarrRelease>,
    watermark_advanced: bool,
}

/// Fetch recent releases from Prowlarr, paginating until we overlap the watermark.
///
/// Pagination stops when any of these conditions are met:
/// - The oldest item in the batch overlaps the watermark (normal case)
/// - Sonarr-style safety cap: >=1000 results AND >24h old, or >7 days absolute
/// - Fewer than a full page returned (no more results)
/// - No watermark exists (first run, one page is enough)
///
/// Indexers currently in retry-after (from previous 429 responses) are skipped.
/// New 429s are recorded in `retry_after_map` and in the DB via `record_rate_limited()`.
#[allow(clippy::too_many_arguments)]
async fn fetch_recent_releases(
    pool: &sqlx::PgPool,
    source_id: uuid::Uuid,
    client: &ProwlarrClient,
    indexer_ids: &[i32],
    categories: &[i32],
    watermark: &RssWatermark,
    retry_after_map: &mut HashMap<String, String>,
) -> Result<FetchResult, Box<dyn std::error::Error + Send + Sync>> {
    const RSS_PAGE_SIZE: u32 = 100;

    let now = OffsetDateTime::now_utc();

    // Expire old retry-after entries
    retry_after_map.retain(|indexer_id, until_str| {
        let still_active =
            OffsetDateTime::parse(until_str, &time::format_description::well_known::Rfc3339)
                .is_ok_and(|until| until > now);
        if !still_active {
            debug!(indexer_id = %indexer_id, "Retry-after expired, re-enabling indexer");
        }
        still_active
    });

    // Determine which indexers to fetch from
    let target_indexer_ids: Vec<i32> = if !indexer_ids.is_empty() {
        indexer_ids.to_vec()
    } else {
        match client.list_indexers().await {
            Ok(indexers) => indexers
                .into_iter()
                .filter(|i| i.enable)
                .map(|i| i.id)
                .collect(),
            Err(e) => {
                error!(error = %e, "Failed to list Prowlarr indexers");
                return Err(Box::new(e));
            }
        }
    };

    if target_indexer_ids.is_empty() {
        debug!("No indexers to fetch from");
        return Ok(FetchResult {
            releases: Vec::new(),
            watermark_advanced: false,
        });
    }

    debug!(
        indexer_ids = ?target_indexer_ids,
        "Fetching releases from indexers"
    );

    // Track which indexers got long rate limits for proactive watermark advance
    let mut long_rate_limited_count = 0;
    let mut total_eligible_indexers = 0;

    // Fetch from each indexer with pagination
    let mut all_releases = Vec::new();
    for indexer_id in &target_indexer_ids {
        let id_str = indexer_id.to_string();

        // Skip indexers in retry-after
        if retry_after_map.contains_key(&id_str) {
            debug!(
                indexer_id = indexer_id,
                "Skipping indexer due to active retry-after"
            );
            continue;
        }

        total_eligible_indexers += 1;
        let mut indexer_releases = Vec::new();
        let mut page: u32 = 0;
        let mut prev_oldest_guid: Option<String> = None;

        loop {
            // Throttle between pages (skip delay for first page)
            if page > 0 {
                tokio::time::sleep(PAGINATION_DELAY).await;
            }

            let offset = page * RSS_PAGE_SIZE;
            match client
                .fetch_rss(*indexer_id, categories, RSS_PAGE_SIZE, offset)
                .await
            {
                Ok(releases) => {
                    let count = releases.len();
                    debug!(
                        indexer_id = indexer_id,
                        page = page + 1,
                        count = count,
                        offset = offset,
                        "Fetched releases from indexer"
                    );

                    // Detect indexers that ignore offset (return same page)
                    let new_oldest_guid = releases.last().map(|r| r.guid.clone());
                    if page > 0 && new_oldest_guid == prev_oldest_guid {
                        warn!(
                            indexer_id = indexer_id,
                            "Indexer appears to ignore offset parameter, stopping pagination"
                        );
                        break;
                    }
                    prev_oldest_guid = new_oldest_guid;

                    indexer_releases.extend(releases);

                    // Stop if fewer than a full page (no more results) or no watermark
                    if (count as u32) < RSS_PAGE_SIZE || watermark.last_seen_publish_date.is_none()
                    {
                        break;
                    }

                    // Check if oldest item overlaps the watermark
                    let oldest = &indexer_releases[indexer_releases.len() - 1];
                    let overlaps = date_is_at_or_before(
                        &oldest.publish_date,
                        watermark.last_seen_publish_date.as_ref().unwrap(),
                    );
                    if overlaps {
                        debug!(
                            indexer_id = indexer_id,
                            pages_fetched = page + 1,
                            "Reached watermark overlap, stopping pagination"
                        );
                        break;
                    }

                    // Sonarr-style safety cap
                    if let Some(oldest_time) = parse_publish_date(&oldest.publish_date) {
                        let age = now - oldest_time;
                        if age.whole_seconds() > HARD_CATCHUP_SECONDS {
                            info!(
                                indexer_id = indexer_id,
                                pages_fetched = page + 1,
                                oldest_age_hours = age.whole_seconds() / 3600,
                                "Hit hard catchup cap (7d), stopping pagination"
                            );
                            break;
                        }
                        if age.whole_seconds() > MAX_CATCHUP_SECONDS
                            && indexer_releases.len() >= CATCHUP_RESULT_THRESHOLD
                        {
                            info!(
                                indexer_id = indexer_id,
                                pages_fetched = page + 1,
                                total_releases = indexer_releases.len(),
                                oldest_age_hours = age.whole_seconds() / 3600,
                                "Hit catchup cap (>24h and >=1000 results), stopping pagination"
                            );
                            break;
                        }
                    }

                    debug!(
                        indexer_id = indexer_id,
                        page = page + 1,
                        oldest_publish_date = %oldest.publish_date,
                        watermark = ?watermark.last_seen_publish_date,
                        "Oldest item still newer than watermark, fetching next page"
                    );
                }
                Err(ProwlarrError::RateLimited(retry_after_secs)) => {
                    let capped = retry_after_secs.map(|s| s.min(MAX_RETRY_AFTER_SECONDS));
                    let until = now
                        + time::Duration::seconds(
                            capped.unwrap_or(3600) as i64, // default 1h
                        );
                    let until_str = until
                        .format(&time::format_description::well_known::Rfc3339)
                        .unwrap_or_default();
                    warn!(
                        indexer_id = indexer_id,
                        retry_after_secs = ?capped,
                        retry_until = %until_str,
                        "Indexer returned 429, backing off"
                    );
                    retry_after_map.insert(id_str.clone(), until_str);

                    // Track long rate limits for proactive watermark advance
                    if capped.unwrap_or(3600) >= PROACTIVE_ADVANCE_THRESHOLD_SECS {
                        long_rate_limited_count += 1;
                    }
                    break;
                }
                Err(e) => {
                    error!(
                        indexer_id = indexer_id,
                        page = page + 1,
                        error = %e,
                        "Failed to fetch from indexer, continuing with others"
                    );
                    break;
                }
            }

            page += 1;
        }

        all_releases.extend(indexer_releases);
    }

    // If ALL eligible indexers got long rate limits, record source-level rate limit
    // and proactively advance watermark.
    let watermark_advanced =
        total_eligible_indexers > 0 && long_rate_limited_count >= total_eligible_indexers;
    if watermark_advanced {
        warn!(
            source_id = %source_id,
            long_rate_limited = long_rate_limited_count,
            total_eligible = total_eligible_indexers,
            old_watermark = ?watermark.last_seen_publish_date,
            "All indexers rate limited for >6h — advancing watermark to now. \
             Releases may have been missed; backlog search will attempt recovery."
        );
        // Record source-level rate limit so search also backs off
        let _ = watch_repo::record_rate_limited(pool, source_id, None).await;
    }

    info!(
        total_releases = all_releases.len(),
        "Finished fetching releases from all indexers"
    );

    Ok(FetchResult {
        releases: all_releases,
        watermark_advanced,
    })
}

/// Convert a Prowlarr release to our generic Release format
fn prowlarr_to_release(
    release: &ProwlarrRelease,
    source_name: &str,
    source_id: uuid::Uuid,
) -> Release {
    // Extract category IDs
    let category_ids: Vec<i32> = release
        .categories
        .as_ref()
        .map(|cats| cats.iter().map(|c| c.id).collect())
        .unwrap_or_default();

    // Determine protocol
    let protocol = release.protocol.clone().unwrap_or_else(|| {
        if release.magnet_url.is_some() {
            "torrent".to_string()
        } else {
            "usenet".to_string()
        }
    });

    Release {
        guid: release.guid.clone(),
        title: release.title.clone(),
        file_name: release.file_name.clone(),
        download_url: release.download_url.clone(),
        magnet_url: release.magnet_url.clone(),
        info_url: release.info_url.clone(),
        size: release.size,
        publish_date: release.publish_date.clone(),
        category_ids,
        indexer: source_name.to_string(),
        protocol,
        info_hash: release.info_hash.clone(),
        files: release.files,
        seeders: release.seeders,
        peers: release.peers,
        leechers: release.leechers,
        grabs: release.grabs,
        votes: release.votes,
        indexer_id: release.indexer_id,
        indexer_source_id: Some(source_id),
        raw_data: serde_json::to_value(release).unwrap_or_default(),
        external_ids: Vec::new(),
        media: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_parsing_unprefixed() {
        let json = serde_json::json!({
            "base_url": "http://prowlarr:9696",
            "api_key": "test-key",
            "indexer_ids": [1, 2, 3],
            "poll_interval_minutes": 30
        });

        let config: ProwlarrRssConfig = serde_json::from_value(json).unwrap();
        assert_eq!(config.base_url, "http://prowlarr:9696");
        assert_eq!(config.api_key, "test-key");
        assert_eq!(config.indexer_ids, vec![1, 2, 3]);
        assert_eq!(config.poll_interval_minutes, 30);
    }

    #[test]
    fn test_config_parsing_prefixed_alias() {
        let json = serde_json::json!({
            "prowlarr_base_url": "http://prowlarr:9696",
            "prowlarr_api_key": "test-key",
            "prowlarr_indexer_id": 42,
        });

        let config: ProwlarrRssConfig = serde_json::from_value(json).unwrap();
        assert_eq!(config.base_url, "http://prowlarr:9696");
        assert_eq!(config.api_key, "test-key");
        assert_eq!(config.indexer_id, Some(42));
        assert!(config.indexer_ids.is_empty());
    }

    #[test]
    fn test_config_defaults() {
        let json = serde_json::json!({
            "base_url": "http://prowlarr:9696",
            "api_key": "test-key"
        });

        let config: ProwlarrRssConfig = serde_json::from_value(json).unwrap();
        assert!(config.indexer_ids.is_empty());
        assert_eq!(config.indexer_id, None);
        assert_eq!(config.poll_interval_minutes, 15);
    }
}
