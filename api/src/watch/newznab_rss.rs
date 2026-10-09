//! RSS handler for native indexer sources (newznab, torznab, cardigann).
//!
//! Deduplication is handled per-pattern by `watch_matches` table, not here.
//!
//! Watermarking: Tracks the newest seen GUID and publish_date to detect gaps.
//! Pagination: Fetches additional pages with Sonarr-style safety cap.
//! 429 handling: Records rate-limited state in DB via `record_rate_limited()`.
//! The caller checks `should_skip_source()` before calling this handler.

use time::OffsetDateTime;
use tracing::{debug, info, warn};

use crate::indexers::{IndexerContext, IndexerResolver};
use crate::repositories::watch as watch_repo;

use super::types::{Release, RssCheckResult, RssWatermark};

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

/// Page size for RSS pagination.
const RSS_PAGE_SIZE: u32 = 100;

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

/// Check a native indexer source for new releases via RSS with watermark tracking.
///
/// Paginates until watermark overlap or Sonarr-style safety cap.
/// On 429, records rate-limited state in DB so all code paths respect it.
///
/// The caller must check `should_skip_source()` before calling this —
/// rate-limited sources are blocked at that level, not here.
pub async fn check_native_rss_with_watermark(
    pool: &sqlx::PgPool,
    http_client: &reqwest::Client,
    indexers: &dyn IndexerResolver,
    source: &watch_repo::IndexerSourceRow,
) -> Result<RssCheckResult, Box<dyn std::error::Error + Send + Sync>> {
    let handler = indexers
        .get(&source.handler_type)
        .ok_or_else(|| format!("{} handler not registered", source.handler_type))?;

    let ctx = IndexerContext { pool, http_client };

    // Load existing watermark from source state
    let watermark: RssWatermark = source
        .state
        .as_ref()
        .and_then(|v| serde_json::from_value(v.clone()).ok())
        .unwrap_or_default();

    let now = OffsetDateTime::now_utc();

    info!(
        source_id = %source.id,
        source_name = %source.name,
        handler_type = %source.handler_type,
        "Checking source for new releases"
    );

    // Paginated fetch
    let mut all_releases: Vec<Release> = Vec::new();
    let mut page: u32 = 0;
    let mut prev_oldest_guid: Option<String> = None;
    let mut watermark_advanced = false;

    loop {
        // Throttle between pages (skip delay for first page)
        if page > 0 {
            tokio::time::sleep(PAGINATION_DELAY).await;
        }

        let offset = page * RSS_PAGE_SIZE;
        match handler.rss_page(&ctx, source, offset, RSS_PAGE_SIZE).await {
            Ok(result) => {
                let count = result.releases.len();
                debug!(
                    source = %source.name,
                    page = page + 1,
                    count = count,
                    offset = offset,
                    "Fetched RSS page"
                );

                // Detect indexers that ignore offset (return same page)
                let new_oldest_guid = result.releases.last().map(|r| r.guid.clone());
                if page > 0 && new_oldest_guid == prev_oldest_guid {
                    warn!(
                        source = %source.name,
                        "Indexer appears to ignore offset parameter, stopping pagination"
                    );
                    break;
                }
                prev_oldest_guid = new_oldest_guid;

                all_releases.extend(result.releases);

                // Stop if fewer than a full page, no watermark, or indexer doesn't support pagination
                if (count as u32) < RSS_PAGE_SIZE
                    || watermark.last_seen_publish_date.is_none()
                    || !result.supports_pagination
                {
                    break;
                }

                // Check if oldest item overlaps the watermark
                let oldest = all_releases
                    .iter()
                    .min_by(|a, b| cmp_publish_dates(&a.publish_date, &b.publish_date))
                    .unwrap();
                let overlaps = date_is_at_or_before(
                    &oldest.publish_date,
                    watermark.last_seen_publish_date.as_ref().unwrap(),
                );
                if overlaps {
                    debug!(
                        source = %source.name,
                        pages_fetched = page + 1,
                        "Reached watermark overlap, stopping pagination"
                    );
                    break;
                }

                // Sonarr-style safety cap: hard cap at 7 days, soft cap at
                // 24h when we've also accumulated >= 1000 results.
                if let Some(oldest_time) = parse_publish_date(&oldest.publish_date) {
                    let age = now - oldest_time;
                    if age.whole_seconds() > HARD_CATCHUP_SECONDS {
                        info!(
                            source = %source.name,
                            pages_fetched = page + 1,
                            oldest_age_hours = age.whole_seconds() / 3600,
                            "Hit hard catchup cap (7d), stopping pagination"
                        );
                        break;
                    }
                    if age.whole_seconds() > MAX_CATCHUP_SECONDS
                        && all_releases.len() >= CATCHUP_RESULT_THRESHOLD
                    {
                        info!(
                            source = %source.name,
                            pages_fetched = page + 1,
                            total_releases = all_releases.len(),
                            oldest_age_hours = age.whole_seconds() / 3600,
                            "Hit catchup cap (>24h and >=1000 results), stopping pagination"
                        );
                        break;
                    }
                }

                debug!(
                    source = %source.name,
                    page = page + 1,
                    "Oldest item still newer than watermark, fetching next page"
                );
            }
            Err(e) if e.status == axum::http::StatusCode::TOO_MANY_REQUESTS => {
                let capped = e
                    .retry_after_seconds
                    .map(|s| s.min(MAX_RETRY_AFTER_SECONDS));
                warn!(
                    source = %source.name,
                    retry_after_secs = ?capped,
                    "Source returned 429, recording rate limit"
                );
                // Record in DB — defaults to 1h if no Retry-After header
                let _ = watch_repo::record_rate_limited(pool, source.id, capped).await;

                // Proactive watermark advance: if rate-limited for a long time,
                // advance watermark to now and accept the gap rather than
                // bursting when the rate limit expires.
                if capped.unwrap_or(0) >= PROACTIVE_ADVANCE_THRESHOLD_SECS {
                    warn!(
                        source = %source.name,
                        old_watermark = ?watermark.last_seen_publish_date,
                        retry_hours = capped.unwrap_or(0) / 3600,
                        "Rate limited for >6h — advancing watermark to now. \
                         Releases may have been missed; backlog search will attempt recovery."
                    );
                    watermark_advanced = true;
                }
                break;
            }
            Err(e) => {
                warn!(error = %e, source = %source.name, "RSS fetch failed");
                return Err(Box::new(e));
            }
        }

        page += 1;
    }

    info!(
        source_id = %source.id,
        total_releases = all_releases.len(),
        "Fetched releases from source"
    );

    // If watermark was proactively advanced due to long rate limit, return
    // whatever releases we got (if any) with an advanced watermark.
    if watermark_advanced {
        let now_rfc2822 = now
            .format(&time::format_description::well_known::Rfc2822)
            .unwrap_or_default();
        let new_watermark = Some(RssWatermark {
            last_seen_guid: None,
            last_seen_publish_date: Some(now_rfc2822),
            indexer_retry_after: Default::default(),
        });
        return Ok(RssCheckResult {
            releases: all_releases,
            new_watermark,
            gap_detected: true,
        });
    }

    if all_releases.is_empty() {
        return Ok(RssCheckResult {
            releases: all_releases,
            new_watermark: None,
            gap_detected: false,
        });
    }

    // Find the chronologically newest and oldest across all results
    let newest = all_releases
        .iter()
        .max_by(|a, b| cmp_publish_dates(&a.publish_date, &b.publish_date))
        .unwrap();
    let oldest = all_releases
        .iter()
        .min_by(|a, b| cmp_publish_dates(&a.publish_date, &b.publish_date))
        .unwrap();

    // Detect gap: if we have a watermark and the oldest returned item is newer than it
    let gap_detected = if let Some(ref last_date) = watermark.last_seen_publish_date {
        if date_is_newer(&oldest.publish_date, last_date) {
            warn!(
                source = %source.name,
                oldest_returned = %oldest.publish_date,
                last_seen = %last_date,
                "Gap detected in RSS feed - some releases may have been missed. Backlog search will catch up."
            );
            true
        } else {
            false
        }
    } else {
        false
    };

    // Update watermark to newest seen
    let new_watermark = Some(RssWatermark {
        last_seen_guid: Some(newest.guid.clone()),
        last_seen_publish_date: Some(newest.publish_date.clone()),
        indexer_retry_after: Default::default(),
    });

    Ok(RssCheckResult {
        releases: all_releases,
        new_watermark,
        gap_detected,
    })
}

/// Check a native indexer source for new releases via RSS (without watermark tracking).
///
/// Simple wrapper that discards watermark info. Use `check_native_rss_with_watermark`
/// if you need gap detection.
pub async fn check_native_rss(
    pool: &sqlx::PgPool,
    http_client: &reqwest::Client,
    indexers: &dyn IndexerResolver,
    source: &watch_repo::IndexerSourceRow,
) -> Result<Vec<Release>, Box<dyn std::error::Error + Send + Sync>> {
    let result = check_native_rss_with_watermark(pool, http_client, indexers, source).await?;
    Ok(result.releases)
}
