//! StremThru torznab hash lookup client.
//!
//! Replaces the defunct Zilean ElfHosted instance. StremThru provides a torznab-compatible
//! API that aggregates torrent metadata including DMM hashlists.

use reqwest::Client;
use std::time::Duration;
use tracing::warn;

use crate::parsing::{ParsedMedia, parse_release};
use crate::repositories::hash_index::HashIndexEntry;

const BASE_URL: &str = "https://stremthru.13377001.xyz";
const TIMEOUT: Duration = Duration::from_secs(3);

pub struct ZileanClient {
    client: Client,
}

impl ZileanClient {
    pub fn new(client: Client) -> Self {
        Self { client }
    }

    /// Search StremThru torznab by text query. Returns parsed hash index entries.
    pub async fn search(&self, query: &str) -> Vec<HashIndexEntry> {
        match self.search_inner(query).await {
            Ok(entries) => entries,
            Err(e) => {
                warn!(query, error = %e, "stremthru search failed");
                Vec::new()
            }
        }
    }

    async fn search_inner(
        &self,
        query: &str,
    ) -> Result<Vec<HashIndexEntry>, Box<dyn std::error::Error + Send + Sync>> {
        // Torznab search: t=search for text, cat=2000 for movies, cat=5000 for TV
        // We search without category to get both
        let url = format!("{BASE_URL}/v0/torznab/api");
        let resp = self
            .client
            .get(&url)
            .query(&[("t", "search"), ("q", query)])
            .timeout(TIMEOUT)
            .send()
            .await?
            .error_for_status()?;

        let body = resp.text().await?;
        parse_torznab_response(&body)
    }
}

/// Parse a torznab XML response into HashIndexEntry entries.
fn parse_torznab_response(
    xml: &str,
) -> Result<Vec<HashIndexEntry>, Box<dyn std::error::Error + Send + Sync>> {
    // Torznab returns RSS-like XML with <item> elements
    // Each item has: <title>, <size>, <attr name="infohash" value="...">, etc.
    let mut entries = Vec::new();

    // Simple XML parsing — find each <item>...</item>
    for item_str in xml.split("<item>").skip(1) {
        let item_end = item_str.find("</item>").unwrap_or(item_str.len());
        let item = &item_str[..item_end];

        let title = extract_xml_value(item, "title").unwrap_or_default();
        if title.is_empty() {
            continue;
        }

        let info_hash = extract_torznab_attr(item, "infohash")
            .or_else(|| extract_torznab_attr(item, "info_hash"))
            .unwrap_or_default()
            .to_lowercase();

        if info_hash.len() != 40 {
            continue;
        }

        let size: Option<i64> = extract_xml_value(item, "size").and_then(|s| s.parse().ok());
        let imdb_id = extract_torznab_attr(item, "imdbid")
            .map(|id| {
                if id.starts_with("tt") {
                    id
                } else {
                    format!("tt{id}")
                }
            })
            .filter(|id| id.len() > 2);

        let parsed = parse_release(&title, "movies");
        let year = match &parsed.media {
            ParsedMedia::Movie(m) => m.year.and_then(|y| i16::try_from(y).ok()),
            _ => None,
        };
        let quality = serde_json::to_value(&parsed.quality).ok();
        let (season, episodes) = crate::repositories::hash_index::derive_season_episodes(&title);

        entries.push(HashIndexEntry {
            info_hash,
            imdb_id,
            tmdb_id: None,
            original_title: title,
            parsed_title: Some(parsed.title),
            year,
            quality,
            size,
            magnet_url: None,
            source: "stremthru".to_string(),
            seeders: extract_torznab_attr(item, "seeders").and_then(|s| s.parse().ok()),
            season,
            episodes,
        });
    }

    Ok(entries)
}

/// Extract the text content of a simple XML element like <tag>value</tag>.
fn extract_xml_value(xml: &str, tag: &str) -> Option<String> {
    let open = format!("<{tag}>");
    let close = format!("</{tag}>");
    let start = xml.find(&open)?;
    let value_start = start + open.len();
    let end = xml[value_start..].find(&close)?;
    let value = &xml[value_start..value_start + end];

    // Handle CDATA
    let value = value
        .strip_prefix("<![CDATA[")
        .and_then(|v| v.strip_suffix("]]>"))
        .unwrap_or(value);

    Some(value.to_string())
}

/// Extract a torznab:attr value: <torznab:attr name="key" value="val"/>
fn extract_torznab_attr(xml: &str, name: &str) -> Option<String> {
    // Also handle newznab:attr
    for prefix in &["torznab:attr", "newznab:attr"] {
        let pattern = format!("<{prefix} name=\"{name}\" value=\"");
        if let Some(start) = xml.find(&pattern) {
            let value_start = start + pattern.len();
            if let Some(end) = xml[value_start..].find('"') {
                return Some(xml[value_start..value_start + end].to_string());
            }
        }
    }
    None
}
