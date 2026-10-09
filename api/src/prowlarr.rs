use reqwest::{Client, Url};
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};

use crate::watch::Release;

#[derive(Debug, thiserror::Error)]
pub enum ProwlarrError {
    #[error("Invalid base URL: {0}")]
    InvalidUrl(String),
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),
    #[error("Unexpected response status: {0}")]
    Status(reqwest::StatusCode),
    #[error("Rate limited (429), retry after {0:?}s")]
    RateLimited(Option<u64>),
}

#[derive(Clone)]
pub struct ProwlarrClient {
    base_url: String,
    api_key: String,
    http: Client,
}

impl ProwlarrClient {
    pub fn new(base_url: String, api_key: String) -> Self {
        let http = crate::http::new_client();
        Self {
            base_url,
            api_key,
            http,
        }
    }

    pub fn new_with_client(base_url: String, api_key: String, http: Client) -> Self {
        Self {
            base_url,
            api_key,
            http,
        }
    }

    fn build_url(&self, path: &str) -> Result<Url, ProwlarrError> {
        let base = self.base_url.trim_end_matches('/');
        let url = format!("{base}{path}");
        Url::parse(&url).map_err(|_| ProwlarrError::InvalidUrl(url))
    }

    /// List all configured indexers from Prowlarr
    pub async fn list_indexers(&self) -> Result<Vec<ProwlarrIndexer>, ProwlarrError> {
        let url = self.build_url("/api/v1/indexer")?;

        info!(url = %url, "Fetching Prowlarr indexers");

        let resp = self
            .http
            .get(url)
            .header("X-Api-Key", &self.api_key)
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(ProwlarrError::Status(resp.status()));
        }

        let body = resp.json::<Vec<ProwlarrIndexer>>().await?;
        info!(indexer_count = body.len(), "Fetched Prowlarr indexers");
        Ok(body)
    }

    /// Fetch RSS feed from a specific indexer via Prowlarr's Newznab proxy.
    ///
    /// Uses the actual RSS endpoint (`t=rss` or `t=search` for caps) which is
    /// designed for polling - it returns pre-generated feeds rather than
    /// triggering active searches on the indexer.
    pub async fn fetch_rss(
        &self,
        indexer_id: i32,
        categories: &[i32],
        limit: u32,
        offset: u32,
    ) -> Result<Vec<ProwlarrRelease>, ProwlarrError> {
        // Use Prowlarr's Newznab proxy RSS endpoint
        let path = format!("/api/v1/indexer/{indexer_id}/newznab");
        let mut url = self.build_url(&path)?;
        {
            let mut params = url.query_pairs_mut();
            params.append_pair("t", "search"); // Newznab RSS-style endpoint
            params.append_pair("limit", &limit.to_string());
            params.append_pair("offset", &offset.to_string());
            for category in categories {
                params.append_pair("cat", &category.to_string());
            }
        }

        info!(
            url = %url,
            indexer_id = indexer_id,
            categories = ?categories,
            limit = limit,
            "Fetching RSS from indexer via Newznab endpoint"
        );

        let resp = self
            .http
            .get(url)
            .header("X-Api-Key", &self.api_key)
            .send()
            .await?;

        let status = resp.status();
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let retry_after = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok());
            return Err(ProwlarrError::RateLimited(retry_after));
        }
        if !status.is_success() {
            info!(status = %status, indexer_id = indexer_id, "Prowlarr RSS fetch failed");
            return Err(ProwlarrError::Status(status));
        }

        // Parse the RSS/XML response
        let body = resp.text().await?;
        let releases = parse_newznab_rss(&body, indexer_id)?;

        info!(
            indexer_id = indexer_id,
            result_count = releases.len(),
            "Fetched RSS releases from indexer"
        );
        Ok(releases)
    }

    pub async fn search(
        &self,
        query: &str,
        categories: &[i32],
    ) -> Result<Vec<ProwlarrRelease>, ProwlarrError> {
        self.search_with_limit(query, categories, 100).await
    }

    pub async fn search_with_limit(
        &self,
        query: &str,
        categories: &[i32],
        limit: u32,
    ) -> Result<Vec<ProwlarrRelease>, ProwlarrError> {
        let mut url = self.build_url("/api/v1/search")?;
        {
            let mut params = url.query_pairs_mut();
            params.append_pair("query", query);
            params.append_pair("type", "search");
            params.append_pair("limit", &limit.to_string());
            params.append_pair("offset", "0");
            for category in categories {
                params.append_pair("categories", &category.to_string());
            }
        }

        info!(
            url = %url,
            query = %query,
            categories = ?categories,
            limit = limit,
            "Prowlarr search request"
        );

        let resp = self
            .http
            .get(url.clone())
            .header("X-Api-Key", &self.api_key)
            .header("X-Prowlarr-Client", "true")
            .send()
            .await?;

        let status = resp.status();
        if !status.is_success() {
            info!(status = %status, "Prowlarr search failed");
            return Err(ProwlarrError::Status(status));
        }

        let body = resp.json::<Vec<ProwlarrRelease>>().await?;
        info!(result_count = body.len(), "Prowlarr search completed");
        Ok(body)
    }

    pub async fn download_clients(&self) -> Result<Vec<DownloadClient>, ProwlarrError> {
        let url = self.build_url("/api/v1/downloadclient")?;
        let resp = self
            .http
            .get(url)
            .header("X-Api-Key", &self.api_key)
            .send()
            .await?;

        if !resp.status().is_success() {
            return Err(ProwlarrError::Status(resp.status()));
        }

        let body = resp.json::<Vec<DownloadClient>>().await?;
        Ok(body)
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ProwlarrIndexerField {
    pub name: String,
    pub value: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct DownloadClient {
    pub id: i32,
    pub name: Option<String>,
    pub implementation: Option<String>,
    pub enable: bool,
    pub protocol: Option<String>,
    #[serde(default)]
    pub fields: Vec<ProwlarrIndexerField>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ProwlarrIndexer {
    pub id: i32,
    pub name: String,
    pub enable: bool,
    pub protocol: String,
    pub priority: i32,
    #[serde(default)]
    pub supports_rss: bool,
    #[serde(default)]
    pub supports_search: bool,
    pub implementation: Option<String>,
    pub implementation_name: Option<String>,
    #[serde(default)]
    pub categories: Vec<ProwlarrCategory>,
    #[serde(default)]
    pub fields: Vec<ProwlarrIndexerField>,
    pub definition_name: Option<String>,
    /// Privacy level: "public", "private", "semiPrivate" (from Prowlarr API).
    #[serde(default)]
    pub privacy: Option<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ProwlarrRelease {
    pub guid: String,
    pub title: String,
    pub size: i64,
    pub publish_date: String,
    pub indexer: String,
    pub download_url: Option<String>,
    pub magnet_url: Option<String>,
    pub info_url: Option<String>,
    pub seeders: Option<i32>,
    pub leechers: Option<i32>,
    pub peers: Option<i32>,
    pub grabs: Option<i32>,
    pub votes: Option<i32>,
    pub protocol: Option<String>,
    pub indexer_id: Option<i32>,
    pub categories: Option<Vec<ProwlarrCategory>>,
    // New fields for generic SearchResult support
    pub file_name: Option<String>,
    pub info_hash: Option<String>,
    pub files: Option<i32>,
    // Capture any extra fields for passthrough/debugging if needed
    #[serde(flatten)]
    pub other: serde_json::Map<String, serde_json::Value>,
}

impl From<ProwlarrRelease> for Release {
    fn from(r: ProwlarrRelease) -> Self {
        let raw_data = serde_json::to_value(&r).unwrap_or_default();
        let external_ids = extract_external_ids_from_other(&r.other);
        Release {
            guid: r.guid,
            title: r.title,
            file_name: r.file_name,
            download_url: r.download_url,
            magnet_url: r.magnet_url,
            info_url: r.info_url,
            size: r.size,
            publish_date: r.publish_date,
            category_ids: r
                .categories
                .map(|cats| cats.into_iter().map(|c| c.id).collect())
                .unwrap_or_default(),
            indexer: r.indexer,
            protocol: r.protocol.unwrap_or_else(|| "unknown".to_string()),
            info_hash: r.info_hash,
            files: r.files,
            seeders: r.seeders,
            peers: r.peers,
            leechers: r.leechers,
            grabs: r.grabs,
            votes: r.votes,
            indexer_id: r.indexer_id,
            indexer_source_id: None,
            raw_data,
            external_ids,
            media: None,
        }
    }
}

/// Extract external IDs from Prowlarr's `other` fields.
fn extract_external_ids_from_other(
    other: &serde_json::Map<String, serde_json::Value>,
) -> Vec<crate::watch::ExternalId> {
    use crate::watch::{ExternalId, ExternalIdSource};
    let mut ids = Vec::new();

    if let Some(val) = other.get("imdbId").and_then(|v| v.as_str())
        && !val.is_empty()
    {
        ids.push(ExternalId {
            source: ExternalIdSource::Imdb,
            id: val.to_string(),
        });
    }
    if let Some(val) = other.get("tmdbId").and_then(|v| v.as_i64())
        && val > 0
    {
        ids.push(ExternalId {
            source: ExternalIdSource::Tmdb,
            id: val.to_string(),
        });
    }
    if let Some(val) = other.get("tvdbId").and_then(|v| v.as_i64())
        && val > 0
    {
        ids.push(ExternalId {
            source: ExternalIdSource::Tvdb,
            id: val.to_string(),
        });
    }
    if let Some(val) = other.get("tvMazeId").and_then(|v| v.as_i64())
        && val > 0
    {
        ids.push(ExternalId {
            source: ExternalIdSource::TvMaze,
            id: val.to_string(),
        });
    }

    ids
}

#[derive(Debug, Deserialize, Serialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ProwlarrCategory {
    pub id: i32,
    pub name: Option<String>, // Some internal Prowlarr categories (107xxx) don't have names
    #[serde(default)]
    pub sub_categories: Vec<ProwlarrCategory>,
}

/// Decode common HTML entities in URLs.
/// Newznab RSS feeds sometimes return HTML-encoded URLs (e.g., `&amp;` instead of `&`).
fn decode_html_entities_in_url(url: &str) -> String {
    url.replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
}

/// Parse a Newznab RSS XML feed into ProwlarrRelease structs.
///
/// Newznab feeds follow this structure:
/// ```xml
/// <rss>
///   <channel>
///     <item>
///       <title>...</title>
///       <guid>...</guid>
///       <link>...</link>
///       <pubDate>...</pubDate>
///       <enclosure url="..." length="..." type="..."/>
///       <newznab:attr name="category" value="7020"/>
///       <newznab:attr name="size" value="123456"/>
///       ...
///     </item>
///   </channel>
/// </rss>
/// ```
pub(crate) fn parse_newznab_rss(
    xml: &str,
    indexer_id: i32,
) -> Result<Vec<ProwlarrRelease>, ProwlarrError> {
    use quick_xml::Reader;
    use quick_xml::events::Event;

    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut releases = Vec::new();
    let mut in_item = false;

    // Current item fields
    let mut title = String::new();
    let mut guid = String::new();
    let mut link = String::new();
    let mut pub_date = String::new();
    let mut size: i64 = 0;
    let mut categories: Vec<i32> = Vec::new();
    let mut seeders: Option<i32> = None;
    let mut leechers: Option<i32> = None;
    let mut grabs: Option<i32> = None;
    let mut votes: Option<i32> = None;
    let mut info_hash: Option<String> = None;
    let mut files: Option<i32> = None;
    let mut current_element = String::new();
    let mut enclosure_url: Option<String> = None;
    let mut imdb_id: Option<String> = None;
    let mut tmdb_id: Option<String> = None;
    let mut tvdb_id: Option<String> = None;

    let indexer_name = format!("Indexer {}", indexer_id);

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if name == "item" {
                    in_item = true;
                    // Reset item fields
                    title.clear();
                    guid.clear();
                    link.clear();
                    pub_date.clear();
                    size = 0;
                    categories.clear();
                    seeders = None;
                    leechers = None;
                    grabs = None;
                    votes = None;
                    info_hash = None;
                    files = None;
                    enclosure_url = None;
                    imdb_id = None;
                    tmdb_id = None;
                    tvdb_id = None;
                } else if in_item {
                    current_element = name;
                }
            }
            Ok(Event::Empty(e)) => {
                if !in_item {
                    continue;
                }
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();

                // Handle <enclosure url="..." length="..." />
                if name == "enclosure" {
                    for attr in e.attributes().flatten() {
                        let key = String::from_utf8_lossy(attr.key.as_ref()).to_string();
                        let val = String::from_utf8_lossy(&attr.value).to_string();
                        match key.as_str() {
                            "url" => enclosure_url = Some(val),
                            "length" => {
                                if let Ok(s) = val.parse() {
                                    size = s;
                                }
                            }
                            _ => {}
                        }
                    }
                }

                // Handle <newznab:attr name="..." value="..." />
                if name.contains("attr") {
                    let mut attr_name = String::new();
                    let mut attr_value = String::new();
                    for attr in e.attributes().flatten() {
                        let key = String::from_utf8_lossy(attr.key.as_ref()).to_string();
                        let val = String::from_utf8_lossy(&attr.value).to_string();
                        if key == "name" {
                            attr_name = val;
                        } else if key == "value" {
                            attr_value = val;
                        }
                    }
                    match attr_name.as_str() {
                        "category" => {
                            if let Ok(cat) = attr_value.parse() {
                                categories.push(cat);
                            }
                        }
                        "size" => {
                            if let Ok(s) = attr_value.parse() {
                                size = s;
                            }
                        }
                        "seeders" => seeders = attr_value.parse().ok(),
                        "leechers" | "peers" => leechers = attr_value.parse().ok(),
                        "grabs" => grabs = attr_value.parse().ok(),
                        "votes" => votes = attr_value.parse().ok(),
                        "infohash" => info_hash = Some(attr_value),
                        "files" => files = attr_value.parse().ok(),
                        "imdb" | "imdbid" if !attr_value.is_empty() => {
                            imdb_id = Some(attr_value);
                        }
                        "tmdb" | "tmdbid" if !attr_value.is_empty() => {
                            tmdb_id = Some(attr_value);
                        }
                        "tvdb" | "tvdbid" | "tvrageid" if !attr_value.is_empty() => {
                            tvdb_id = Some(attr_value);
                        }
                        _ => {}
                    }
                }
            }
            Ok(Event::Text(e)) => {
                if in_item && let Ok(decoded) = e.decode() {
                    match current_element.as_str() {
                        "title" => title.push_str(&decoded),
                        "guid" => guid.push_str(&decoded),
                        "link" => link.push_str(&decoded),
                        "pubDate" => pub_date.push_str(&decoded),
                        _ => {}
                    }
                }
            }
            Ok(Event::GeneralRef(e)) => {
                if in_item && let Ok(decoded) = e.decode() {
                    match current_element.as_str() {
                        "title" => title.push_str(&decoded),
                        "guid" => guid.push_str(&decoded),
                        "link" => link.push_str(&decoded),
                        "pubDate" => pub_date.push_str(&decoded),
                        _ => {}
                    }
                }
            }
            Ok(Event::End(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if name == "item" && in_item {
                    in_item = false;

                    // Build the release
                    if !guid.is_empty() && !title.is_empty() {
                        // Decode HTML entities in URLs — RSS feeds sometimes
                        // return encoded URLs (e.g. &amp; instead of &)
                        if let Some(ref mut u) = enclosure_url {
                            *u = decode_html_entities_in_url(u);
                        }
                        link = decode_html_entities_in_url(&link);

                        // Determine download URL - prefer enclosure, fall back to link
                        let download_url = enclosure_url.clone().or_else(|| {
                            if !link.is_empty() {
                                Some(link.clone())
                            } else {
                                None
                            }
                        });

                        // Determine protocol from URL
                        let protocol = download_url.as_ref().map(|url| {
                            if url.contains("magnet:") || url.contains(".torrent") {
                                "torrent".to_string()
                            } else {
                                "usenet".to_string()
                            }
                        });

                        // Determine if it's a magnet link
                        let (actual_download, magnet) = if download_url
                            .as_ref()
                            .is_some_and(|u| u.starts_with("magnet:"))
                        {
                            (None, download_url.clone())
                        } else {
                            (download_url.clone(), None)
                        };

                        let mut other = serde_json::Map::new();
                        if let Some(ref id) = imdb_id {
                            other.insert(
                                "imdbId".to_string(),
                                serde_json::Value::String(id.clone()),
                            );
                        }
                        if let Some(ref id) = tmdb_id {
                            if let Ok(n) = id.parse::<i64>() {
                                other.insert("tmdbId".to_string(), serde_json::json!(n));
                            } else {
                                other.insert(
                                    "tmdbId".to_string(),
                                    serde_json::Value::String(id.clone()),
                                );
                            }
                        }
                        if let Some(ref id) = tvdb_id {
                            if let Ok(n) = id.parse::<i64>() {
                                other.insert("tvdbId".to_string(), serde_json::json!(n));
                            } else {
                                other.insert(
                                    "tvdbId".to_string(),
                                    serde_json::Value::String(id.clone()),
                                );
                            }
                        }

                        let release = ProwlarrRelease {
                            guid: guid.clone(),
                            title: title.clone(),
                            size,
                            publish_date: pub_date.clone(),
                            indexer: indexer_name.clone(),
                            download_url: actual_download,
                            magnet_url: magnet,
                            info_url: if !link.is_empty() && !link.starts_with("magnet:") {
                                Some(link.clone())
                            } else {
                                None
                            },
                            seeders,
                            leechers,
                            peers: leechers,
                            grabs,
                            votes,
                            protocol,
                            indexer_id: Some(indexer_id),
                            categories: Some(
                                categories
                                    .iter()
                                    .map(|&id| ProwlarrCategory {
                                        id,
                                        name: None,
                                        sub_categories: Vec::new(),
                                    })
                                    .collect(),
                            ),
                            file_name: None,
                            info_hash: info_hash.clone(),
                            files,
                            other,
                        };
                        releases.push(release);
                    } else {
                        debug!(
                            guid = ?guid,
                            title = ?title,
                            "Skipping RSS item with missing guid or title"
                        );
                    }
                }
                current_element.clear();
            }
            Ok(Event::Eof) => break,
            Err(e) => {
                warn!(error = %e, "Error parsing RSS XML");
                // Continue parsing, some items may still be valid
            }
            _ => {}
        }
    }

    Ok(releases)
}

/// API limits parsed from `<newznab:apilimits>` in a Newznab RSS response.
/// Only some indexers return this (e.g., NZBFinder, Usenet Crawler).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NewznabApiLimits {
    pub api_current: i32,
    pub api_max: i32,
    pub grab_current: i32,
    pub grab_max: i32,
}

/// Parse `<newznab:apilimits>` from a Newznab RSS XML response.
/// Returns `None` if the element is not present.
///
/// Format: `<newznab:apilimits apicurrent="115" apimax="5000" grabcurrent="0" grabmax="5000"/>`
pub fn parse_api_limits(xml: &str) -> Option<NewznabApiLimits> {
    use quick_xml::Reader;
    use quick_xml::events::Event;

    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                // Stop scanning once we hit items — apilimits is in <channel> before items
                if name == "item" {
                    return None;
                }
            }
            Ok(Event::Empty(e)) => {
                let name = String::from_utf8_lossy(e.name().as_ref()).to_string();
                if name.contains("apilimits") {
                    let mut api_current = 0;
                    let mut api_max = 0;
                    let mut grab_current = 0;
                    let mut grab_max = 0;

                    for attr in e.attributes().flatten() {
                        let key = String::from_utf8_lossy(attr.key.as_ref()).to_string();
                        let val = String::from_utf8_lossy(&attr.value).to_string();
                        match key.as_str() {
                            "apicurrent" => api_current = val.parse().unwrap_or(0),
                            "apimax" => api_max = val.parse().unwrap_or(0),
                            "grabcurrent" => grab_current = val.parse().unwrap_or(0),
                            "grabmax" => grab_max = val.parse().unwrap_or(0),
                            _ => {}
                        }
                    }

                    if api_max > 0 {
                        return Some(NewznabApiLimits {
                            api_current,
                            api_max,
                            grab_current,
                            grab_max,
                        });
                    }
                }
            }
            Ok(Event::Eof) => return None,
            Err(_) => return None,
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::watch::{ExternalIdSource, Release};
    #[test]
    fn test_newznab_rss_extracts_external_ids() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<rss xmlns:newznab="http://www.newznab.com/DTD/2010/feeds/attributes/">
  <channel>
    <item>
      <title>Test.Release.2024.1080p</title>
      <guid>abc123</guid>
      <link>http://example.com/download/abc123</link>
      <pubDate>Mon, 01 Jan 2024 00:00:00 +0000</pubDate>
      <enclosure url="http://example.com/download/abc123" length="1048576" type="application/x-nzb"/>
      <newznab:attr name="imdb" value="tt1234567"/>
      <newznab:attr name="tmdb" value="98765"/>
      <newznab:attr name="tvdb" value="54321"/>
      <newznab:attr name="category" value="5000"/>
    </item>
  </channel>
</rss>"#;

        let prowlarr_releases = parse_newznab_rss(xml, 1).expect("parse");
        assert_eq!(prowlarr_releases.len(), 1);

        // Convert to Release to check external_ids extraction
        let release: Release = prowlarr_releases[0].clone().into();
        assert_eq!(release.external_ids.len(), 3);
        assert!(
            release
                .external_ids
                .iter()
                .any(|id| id.source == ExternalIdSource::Imdb && id.id == "tt1234567")
        );
        assert!(
            release
                .external_ids
                .iter()
                .any(|id| id.source == ExternalIdSource::Tmdb && id.id == "98765")
        );
        assert!(
            release
                .external_ids
                .iter()
                .any(|id| id.source == ExternalIdSource::Tvdb && id.id == "54321")
        );
    }

    #[test]
    fn test_parse_api_limits_present() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<rss xmlns:newznab="http://www.newznab.com/DTD/2010/feeds/attributes/">
  <channel>
    <newznab:response offset="0" total="50" />
    <newznab:apilimits apicurrent="115" apimax="5000" grabcurrent="3" grabmax="5000" />
    <item>
      <title>Test</title>
      <guid>abc</guid>
    </item>
  </channel>
</rss>"#;

        let limits = parse_api_limits(xml);
        assert!(limits.is_some(), "Expected apilimits to be parsed");
        let limits = limits.unwrap();
        assert_eq!(limits.api_current, 115);
        assert_eq!(limits.api_max, 5000);
        assert_eq!(limits.grab_current, 3);
        assert_eq!(limits.grab_max, 5000);
    }

    #[test]
    fn test_parse_api_limits_absent() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<rss xmlns:newznab="http://www.newznab.com/DTD/2010/feeds/attributes/">
  <channel>
    <newznab:response offset="0" total="50" />
    <item><title>Test</title><guid>abc</guid></item>
  </channel>
</rss>"#;

        assert!(parse_api_limits(xml).is_none());
    }

    #[test]
    fn test_newznab_rss_parses_votes() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<rss xmlns:newznab="http://www.newznab.com/DTD/2010/feeds/attributes/">
  <channel>
    <item>
      <title>Bad.Release.2026.720p.WEB.H264-FAKE</title>
      <guid>bad123</guid>
      <link>http://example.com/download/bad123</link>
      <pubDate>Mon, 25 Mar 2026 12:00:00 +0000</pubDate>
      <enclosure url="http://example.com/download/bad123" length="3000000000" type="application/x-nzb"/>
      <newznab:attr name="category" value="2000"/>
      <newznab:attr name="votes" value="-29"/>
      <newznab:attr name="grabs" value="2018"/>
    </item>
    <item>
      <title>Good.Release.2026.1080p.WEB.H264-GOOD</title>
      <guid>good456</guid>
      <link>http://example.com/download/good456</link>
      <pubDate>Mon, 25 Mar 2026 14:00:00 +0000</pubDate>
      <enclosure url="http://example.com/download/good456" length="5000000000" type="application/x-nzb"/>
      <newznab:attr name="category" value="2000"/>
      <newznab:attr name="votes" value="15"/>
    </item>
    <item>
      <title>No.Votes.2026.1080p.WEB.H264-NONE</title>
      <guid>none789</guid>
      <link>http://example.com/download/none789</link>
      <pubDate>Mon, 25 Mar 2026 16:00:00 +0000</pubDate>
      <enclosure url="http://example.com/download/none789" length="4000000000" type="application/x-nzb"/>
      <newznab:attr name="category" value="2000"/>
    </item>
  </channel>
</rss>"#;

        let releases = parse_newznab_rss(xml, 1).expect("parse");
        assert_eq!(releases.len(), 3);

        // First release has negative votes
        assert_eq!(releases[0].votes, Some(-29));
        assert_eq!(releases[0].grabs, Some(2018));

        // Second release has positive votes
        assert_eq!(releases[1].votes, Some(15));

        // Third release has no votes attribute
        assert_eq!(releases[2].votes, None);

        // Verify votes survive conversion to Release
        let release: Release = releases[0].clone().into();
        assert_eq!(release.votes, Some(-29));
    }

    #[test]
    fn test_extract_external_ids_from_prowlarr_json() {
        let other: serde_json::Map<String, serde_json::Value> =
            serde_json::from_value(serde_json::json!({
                "imdbId": "tt9999999",
                "tmdbId": 42,
                "tvdbId": 0,
                "tvMazeId": 100
            }))
            .unwrap();

        let ids = extract_external_ids_from_other(&other);
        assert_eq!(ids.len(), 3); // tvdbId=0 should be excluded
        assert!(
            ids.iter()
                .any(|id| id.source == ExternalIdSource::Imdb && id.id == "tt9999999")
        );
        assert!(
            ids.iter()
                .any(|id| id.source == ExternalIdSource::Tmdb && id.id == "42")
        );
        assert!(
            ids.iter()
                .any(|id| id.source == ExternalIdSource::TvMaze && id.id == "100")
        );
    }
}
