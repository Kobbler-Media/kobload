//! EZTV TV torrent lookup client (TV only).

use reqwest::Client;
use serde::Deserialize;
use std::time::Duration;
use tracing::warn;

use crate::parsing::{ParsedMedia, parse_release};
use crate::repositories::hash_index::HashIndexEntry;

const BASE_URL: &str = "https://eztv.re/api";
const TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Deserialize)]
struct EztvResponse {
    #[serde(default)]
    torrents: Option<Vec<EztvTorrent>>,
}

#[derive(Debug, Deserialize)]
struct EztvTorrent {
    hash: String,
    #[serde(default)]
    filename: Option<String>,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    size_bytes: Option<StringOrInt>,
    #[serde(default)]
    #[allow(dead_code)]
    season: Option<StringOrInt>,
    #[serde(default)]
    #[allow(dead_code)]
    episode: Option<StringOrInt>,
    #[serde(default)]
    seeds: Option<i32>,
}

/// EZTV returns some numeric fields as either strings or integers.
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum StringOrInt {
    Int(i64),
    Str(String),
}

impl StringOrInt {
    fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Int(n) => Some(*n),
            Self::Str(s) => s.parse().ok(),
        }
    }
}

pub struct EztvClient {
    client: Client,
}

impl EztvClient {
    pub fn new(client: Client) -> Self {
        Self { client }
    }

    /// Look up TV torrents by IMDB ID.
    /// The IMDB ID can include or exclude the "tt" prefix.
    pub async fn search_show(&self, imdb_id: &str) -> Vec<HashIndexEntry> {
        match self.search_show_inner(imdb_id).await {
            Ok(entries) => entries,
            Err(e) => {
                warn!(imdb_id, error = %e, "eztv search failed");
                Vec::new()
            }
        }
    }

    async fn search_show_inner(
        &self,
        imdb_id: &str,
    ) -> Result<Vec<HashIndexEntry>, Box<dyn std::error::Error + Send + Sync>> {
        // EZTV expects numeric IMDB ID without "tt" prefix
        let numeric_id = imdb_id.strip_prefix("tt").unwrap_or(imdb_id);

        let url = format!("{BASE_URL}/get-torrents");
        let resp = self
            .client
            .get(&url)
            .query(&[("imdb_id", numeric_id), ("limit", "100")])
            .timeout(TIMEOUT)
            .send()
            .await?
            .error_for_status()?;

        let data: EztvResponse = resp.json().await?;

        let torrents = match data.torrents {
            Some(t) => t,
            None => return Ok(Vec::new()),
        };

        let full_imdb_id = if imdb_id.starts_with("tt") {
            imdb_id.to_string()
        } else {
            format!("tt{imdb_id}")
        };

        let entries = torrents
            .into_iter()
            .filter(|t| !t.hash.is_empty())
            .map(|t| {
                let filename = t
                    .filename
                    .filter(|f| !f.is_empty())
                    .or_else(|| {
                        // EZTV sometimes has empty filename (e.g. season packs).
                        // Fall back to title, stripping the trailing " EZTV" watermark.
                        t.title
                            .map(|t| t.strip_suffix(" EZTV").unwrap_or(&t).to_string())
                    })
                    .unwrap_or_default();
                let parsed = parse_release(&filename, "tv");
                let quality = serde_json::to_value(&parsed.quality).ok();
                let year = match &parsed.media {
                    ParsedMedia::Movie(m) => m.year.and_then(|y| i16::try_from(y).ok()),
                    _ => None,
                };

                let size = t.size_bytes.and_then(|s| s.as_i64());

                let (season, episodes) =
                    crate::repositories::hash_index::derive_season_episodes_from_media(
                        &parsed.media,
                    );

                HashIndexEntry {
                    info_hash: t.hash.to_lowercase(),
                    imdb_id: Some(full_imdb_id.clone()),
                    tmdb_id: None,
                    original_title: filename,
                    parsed_title: Some(parsed.title),
                    year,
                    quality,
                    size,
                    magnet_url: None,
                    source: "eztv".to_string(),
                    seeders: t.seeds,
                    season,
                    episodes,
                }
            })
            .collect();

        Ok(entries)
    }
}
