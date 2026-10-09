//! Hash index RSS — treats the local torrent hash index as an RSS-like feed
//! for debrid download client integration.
//!
//! This is a built-in virtual source (not user-configured). When a debrid
//! download client is configured, the watch service auto-checks the hash index
//! for new entries matching active patterns.
//!
//! Polls `torrent_hash_index` for entries added/updated since the last watermark,
//! converts them to `Release` structs for the normal watch pattern matching pipeline.

use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use tracing::{debug, info};

use crate::repositories::hash_index;

use super::types::Release;

/// Watermark state persisted in app_settings.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HashIndexWatermark {
    pub last_checked: time::OffsetDateTime,
}

/// Check the hash index for new entries since the last watermark.
/// Returns releases for pattern matching.
pub async fn check_hash_index_rss(
    pool: &PgPool,
    watermark: Option<&HashIndexWatermark>,
) -> Result<(Vec<Release>, HashIndexWatermark), Box<dyn std::error::Error + Send + Sync>> {
    let since = watermark
        .map(|w| w.last_checked)
        .unwrap_or_else(|| time::OffsetDateTime::now_utc() - time::Duration::hours(1));

    let rows = hash_index::search_since(pool, since).await?;

    if rows.is_empty() {
        debug!("Hash index RSS: no new entries since {since}");
        let new_watermark = HashIndexWatermark {
            last_checked: time::OffsetDateTime::now_utc(),
        };
        return Ok((vec![], new_watermark));
    }

    info!(count = rows.len(), "Hash index RSS: found new entries");

    let releases: Vec<Release> = rows
        .iter()
        .filter(|r| !matches!(r.seeders, Some(0)))
        .map(|row| {
            let magnet = row
                .magnet_url
                .clone()
                .unwrap_or_else(|| format!("magnet:?xt=urn:btih:{}", row.info_hash));

            Release {
                guid: format!("hashidx_{}", row.info_hash),
                title: row.original_title.clone(),
                file_name: None,
                download_url: None,
                magnet_url: Some(magnet),
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
                votes: None,
                indexer_id: None,
                indexer_source_id: None, // Built-in source, no DB row
                raw_data: serde_json::json!({
                    "info_hash": row.info_hash,
                    "source": row.source,
                    "seeders": row.seeders,
                }),
                external_ids: Vec::new(),
                media: None,
            }
        })
        .collect();

    let new_watermark = HashIndexWatermark {
        last_checked: time::OffsetDateTime::now_utc(),
    };

    Ok((releases, new_watermark))
}
