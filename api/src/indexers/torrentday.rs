use axum::http::StatusCode;
use serde::Deserialize;

use crate::repositories::watch as watch_repo;
use crate::watch::Release;

use super::types::{
    IndexerCapabilities, IndexerContext, IndexerError, IndexerHandler, IndexerSearchRequest,
};

const DEFAULT_BASE_URL: &str = "https://www.torrentday.com";
const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

#[derive(Debug, Deserialize)]
struct TorrentDayConfig {
    cookie: String,
    #[serde(default)]
    base_url: String,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_bool_lenient"
    )]
    freeleech_only: bool,
}

impl TorrentDayConfig {
    fn base_url(&self) -> &str {
        if self.base_url.is_empty() {
            DEFAULT_BASE_URL
        } else {
            self.base_url.trim_end_matches('/')
        }
    }
}

#[derive(Debug, Deserialize)]
struct TdTorrent {
    #[serde(default, alias = "t")]
    id: i64,
    #[serde(default)]
    name: String,
    #[serde(default)]
    size: i64,
    #[serde(default)]
    seeders: i32,
    #[serde(default)]
    leechers: i32,
    /// Numeric category ID (field name "c" in JSON)
    #[serde(default, alias = "c")]
    category_id: i32,
    /// Unix timestamp
    #[serde(default)]
    ctime: i64,
    #[serde(default)]
    files: Option<i32>,
    #[serde(default)]
    completed: Option<i32>,
    /// IMDB ID (numeric string, e.g. "1264573")
    #[serde(default, rename = "imdb-id")]
    imdb_id: Option<String>,
    /// Download multiplier: 0 = freeleech, 1 = normal
    #[serde(default, rename = "download-multiplier")]
    download_multiplier: Option<f64>,
}

/// Fix invalid JSON escape sequences produced by TorrentDay's API.
///
/// TorrentDay sometimes emits bare backslashes in string values (e.g. torrent
/// names containing `\`). This replaces any `\X` where X is not a valid JSON
/// escape character (`"`, `\`, `/`, `b`, `f`, `n`, `r`, `t`, `u`) with `\\X`,
/// but only inside JSON string values (between unescaped quotes).
fn fix_invalid_json_escapes(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut in_string = false;
    let bytes = input.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let ch = bytes[i];
        if ch == b'"' && (i == 0 || bytes[i - 1] != b'\\') {
            in_string = !in_string;
            out.push('"');
            i += 1;
        } else if in_string && ch == b'\\' && i + 1 < bytes.len() {
            let next = bytes[i + 1];
            if matches!(
                next,
                b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't' | b'u'
            ) {
                // Valid escape — pass through as-is
                out.push('\\');
                out.push(next as char);
                i += 2;
            } else {
                // Invalid escape — double the backslash
                out.push_str("\\\\");
                i += 1;
            }
        } else {
            out.push(ch as char);
            i += 1;
        }
    }
    out
}

/// Check a TorrentDay response for signs of auth failure.
///
/// TD redirects to a login page (HTML) when the cookie is invalid/expired
/// instead of returning 401. reqwest follows the redirect, so we see a 200
/// with HTML content. Detect this and return a clear error message.
fn check_td_auth(resp: &reqwest::Response) -> Result<(), IndexerError> {
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if content_type.contains("text/html") {
        return Err(IndexerError::new(
            StatusCode::UNAUTHORIZED,
            "TorrentDay cookie expired or invalid — got HTML login page instead of JSON. \
             Please update the cookie in indexer settings.",
        ));
    }

    // TD sometimes redirects to /t.json?29;28;... with category params baked in,
    // which returns an empty or unexpected response
    let url = resp.url().to_string();
    if url.contains("login") {
        return Err(IndexerError::new(
            StatusCode::UNAUTHORIZED,
            "TorrentDay redirected to login page — cookie expired. \
             Please update the cookie in indexer settings.",
        ));
    }

    Ok(())
}

pub struct TorrentDayIndexer;

impl TorrentDayIndexer {
    /// Map TorrentDay category IDs to newznab-compatible category IDs.
    /// Cross-referenced against Jackett's authoritative TorrentDay definition.
    fn map_category(cat: i32) -> Vec<i32> {
        match cat {
            // Movies
            1 => vec![2030],   // Movies/XviD
            3 => vec![2040],   // Movies/DVD-R
            5 => vec![2050],   // Movies/Bluray-Full
            11 => vec![2050],  // Movies/Bluray
            13 => vec![2000],  // Movies/Packs
            21 => vec![2030],  // Movies/MP4
            22 => vec![2060],  // Movies/Non-English
            25 => vec![2030],  // Movies/480p
            44 => vec![2030],  // Movies/SD/x264
            48 => vec![2000],  // Movies/x265
            96 => vec![2045],  // Movie/4K (UHD)
            103 => vec![2030], // Movies/Cam
            // TV
            2 => vec![5030],   // TV/XviD
            7 => vec![5040],   // TV/x264
            14 => vec![5000],  // TV/Packs
            24 => vec![5030],  // TV/480p
            26 => vec![5030],  // TV/SD/x264
            30 => vec![5080],  // Documentary
            31 => vec![5030],  // TV/DVD-R
            32 => vec![5040],  // TV/Bluray
            33 => vec![5030],  // TV/DVD-Rip
            34 => vec![5045],  // TV/x265
            46 => vec![5030],  // TV/Mobile
            82 => vec![5060],  // TV/Non-English
            95 => vec![5080],  // Educational
            104 => vec![5045], // TV/4K
            // Games — console
            8 => vec![1020],  // PSP
            9 => vec![1040],  // Xbox
            10 => vec![1010], // Nintendo
            18 => vec![1080], // PS (PlayStation)
            // Games — PC
            4 => vec![4050],  // PC/Games
            40 => vec![4050], // PC/Games (dup)
            // Music
            16 => vec![3060], // Music/Video
            17 => vec![3010], // Music/Audio
            23 => vec![3080], // Music/Non-English
            27 => vec![3000], // Music/Flac
            41 => vec![3000], // Music/Packs
            45 => vec![3020], // Podcast
            // Books
            20 => vec![7000],  // Books
            35 => vec![7020],  // Books/Ebooks
            102 => vec![7020], // Books/Non-English
            // Audiobooks
            42 => vec![3030], // Audio Books
            94 => vec![3030], // Audiobooks
            // Software
            12 => vec![4000], // Software
            28 => vec![4000], // Software/Packs
            43 => vec![4030], // Mac
            // Anime
            29 => vec![5070], // Anime
            // XXX
            6 => vec![6000],  // XXX/Movies
            15 => vec![6070], // XXX/Packs
            19 => vec![6000], // XXX/0Day
            // Other
            47 => vec![8000], // Fonts
            _ => vec![8000],
        }
    }

    /// Map newznab categories to TorrentDay category IDs for search filtering.
    /// Complete reverse mapping of `map_category()`.
    fn newznab_to_td_cats(newznab_cats: &[i32]) -> Vec<i32> {
        let mut td_cats = Vec::new();
        for cat in newznab_cats {
            match cat {
                // Movies (2xxx)
                2000..=2999 => {
                    td_cats.extend_from_slice(&[1, 3, 5, 11, 13, 21, 22, 25, 44, 48, 96, 103]);
                }
                // TV (5xxx, excluding anime 5070-5079 and documentary 5080)
                5000..=5069 | 5081..=5999 => {
                    td_cats.extend_from_slice(&[
                        2, 7, 14, 24, 26, 30, 31, 32, 33, 34, 46, 82, 95, 104,
                    ]);
                }
                5070..=5079 => td_cats.push(29), // Anime
                5080 => td_cats.extend_from_slice(&[30, 95]), // Documentary/Educational
                // Console games (1xxx)
                1000..=1999 => td_cats.extend_from_slice(&[8, 9, 10, 18]),
                // PC games (4050)
                4050 => td_cats.extend_from_slice(&[4, 40]),
                // Music (3000-3029, 3031-3099, not 3030=audiobooks)
                3000..=3029 | 3031..=3099 => {
                    td_cats.extend_from_slice(&[16, 17, 23, 27, 41, 45]);
                }
                3030 => td_cats.extend_from_slice(&[42, 94]), // Audiobooks
                // Books (7xxx)
                7000..=7999 => td_cats.extend_from_slice(&[20, 35, 102]),
                // Software (4xxx except 4050=PC games)
                4000..=4049 | 4051..=4999 => td_cats.extend_from_slice(&[12, 28, 43]),
                // XXX (6xxx)
                6000..=6999 => td_cats.extend_from_slice(&[6, 15, 19]),
                _ => {}
            }
        }
        td_cats.sort_unstable();
        td_cats.dedup();
        td_cats
    }

    fn torrent_to_release(t: TdTorrent, base_url: &str) -> Release {
        let download_url = format!("{}/download.php/{}/{}.torrent", base_url, t.id, t.id);
        let info_url = format!("{}/t/{}", base_url, t.id);
        let category_ids = Self::map_category(t.category_id);

        // Convert unix timestamp to RFC 2822
        let publish_date = time::OffsetDateTime::from_unix_timestamp(t.ctime)
            .ok()
            .and_then(|dt| {
                dt.format(&time::format_description::well_known::Rfc2822)
                    .ok()
            })
            .unwrap_or_default();

        // Extract IMDB external ID if present (TD sends bare numeric like "1264573")
        let mut external_ids = Vec::new();
        if let Some(ref imdb_raw) = t.imdb_id {
            let trimmed = imdb_raw.trim();
            if !trimmed.is_empty() && trimmed != "0" {
                let imdb_id = if trimmed.starts_with("tt") {
                    trimmed.to_string()
                } else {
                    format!("tt{trimmed:0>7}")
                };
                external_ids.push(crate::watch::ExternalId {
                    source: crate::watch::ExternalIdSource::Imdb,
                    id: imdb_id,
                });
            }
        }

        let raw_data = serde_json::json!({
            "td_id": t.id,
            "category": t.category_id,
            "download_multiplier": t.download_multiplier,
        });

        Release {
            guid: format!("td-{}", t.id),
            title: t.name,
            file_name: None,
            download_url: Some(download_url),
            magnet_url: None,
            info_url: Some(info_url),
            size: t.size,
            publish_date,
            category_ids,
            indexer: String::new(),
            protocol: "torrent".to_string(),
            info_hash: None,
            files: t.files,
            seeders: Some(t.seeders),
            peers: Some(t.seeders + t.leechers),
            leechers: Some(t.leechers),
            grabs: t.completed,
            votes: None,
            indexer_id: Some(t.id as i32),
            indexer_source_id: None,
            raw_data,
            external_ids,
            media: None,
        }
    }
}

#[async_trait::async_trait]
impl IndexerHandler for TorrentDayIndexer {
    fn handler_type(&self) -> &'static str {
        "torrentday"
    }

    fn capabilities(&self) -> IndexerCapabilities {
        IndexerCapabilities {
            supports_search: true,
            supports_rss: true,
        }
    }

    fn config_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "fields": [
                {
                    "name": "cookie",
                    "label": "Cookie",
                    "type": "password",
                    "required": true,
                    "placeholder": "uid=...; pass=...",
                    "help": "Sign in to TorrentDay in a browser, open developer tools > \
        Application/Storage > Cookies, and paste the uid and pass values as \"uid=...; pass=...\". \
        Logging out in that browser invalidates the cookie."
                },
                {
                    "name": "base_url",
                    "label": "Base URL",
                    "type": "text",
                    "default": "https://www.torrentday.com",
                    "placeholder": "https://www.torrentday.com"
                },
                {
                    "name": "freeleech_only",
                    "label": "Freeleech only",
                    "type": "checkbox",
                    "default": false
                }
            ]
        })
    }

    async fn search(
        &self,
        ctx: &IndexerContext<'_>,
        source: Option<&watch_repo::IndexerSourceRow>,
        request: IndexerSearchRequest,
    ) -> Result<Vec<Release>, IndexerError> {
        let source = source
            .ok_or_else(|| IndexerError::new(StatusCode::BAD_REQUEST, "indexer source required"))?;

        let config: TorrentDayConfig = serde_json::from_value(source.handler_config.clone())
            .map_err(|e| IndexerError::new(StatusCode::BAD_REQUEST, e.to_string()))?;

        if config.cookie.trim().is_empty() {
            return Err(IndexerError::new(
                StatusCode::BAD_REQUEST,
                "cookie not configured",
            ));
        }

        let base_url = config.base_url();

        // Build category filter string: /c11;c48;... format
        let td_cats = if !request.categories.is_empty() {
            Self::newznab_to_td_cats(&request.categories)
        } else if let Some(ref cats) = source.category_ids {
            cats.clone()
        } else {
            Vec::new()
        };

        let mut url = format!("{}/t.json", base_url);
        let mut query_parts = Vec::new();

        if !td_cats.is_empty() {
            let cat_str = td_cats.iter().map(|c| format!("/c{c}")).collect::<String>();
            query_parts.push(cat_str);
        }

        if config.freeleech_only {
            query_parts.push(";free".to_string());
        }

        if !query_parts.is_empty() {
            url = format!("{}?{}", url, query_parts.join(""));
        }

        let resp = ctx
            .http_client
            .get(&url)
            .query(&[("q", &request.query)])
            .header("Cookie", &config.cookie)
            .header("User-Agent", USER_AGENT)
            .send()
            .await
            .map_err(|e| IndexerError::new(StatusCode::BAD_GATEWAY, e.to_string()))?;

        if !resp.status().is_success() {
            return Err(IndexerError::bad_status("TorrentDay", resp).await);
        }
        check_td_auth(&resp)?;

        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("unknown")
            .to_string();
        let body = resp
            .text()
            .await
            .map_err(|e| IndexerError::new(StatusCode::BAD_GATEWAY, e.to_string()))?;
        let sanitized = fix_invalid_json_escapes(&body);
        let torrents: Vec<TdTorrent> = serde_json::from_str(&sanitized)
            .map_err(|e| IndexerError::json_parse_failed("TorrentDay", &content_type, &body, e))?;

        let mut releases: Vec<Release> = torrents
            .into_iter()
            .map(|t| Self::torrent_to_release(t, base_url))
            .collect();

        for release in &mut releases {
            release.indexer = source.name.clone();
            release.indexer_source_id = Some(source.id);
        }

        Ok(releases)
    }

    async fn rss(
        &self,
        ctx: &IndexerContext<'_>,
        source: &watch_repo::IndexerSourceRow,
    ) -> Result<Vec<Release>, IndexerError> {
        let config: TorrentDayConfig = serde_json::from_value(source.handler_config.clone())
            .map_err(|e| IndexerError::new(StatusCode::BAD_REQUEST, e.to_string()))?;

        if config.cookie.trim().is_empty() {
            return Err(IndexerError::new(
                StatusCode::BAD_REQUEST,
                "cookie not configured",
            ));
        }

        let base_url = config.base_url();

        // Fetch recent torrents (no query = RSS-style feed)
        let mut url = format!("{}/t.json", base_url);

        // Apply category filters if configured
        if let Some(ref cats) = source.category_ids
            && !cats.is_empty()
        {
            let cat_str = cats.iter().map(|c| format!("/c{c}")).collect::<String>();
            url = format!("{url}?{cat_str}");
        }

        let resp = ctx
            .http_client
            .get(&url)
            .header("Cookie", &config.cookie)
            .header("User-Agent", USER_AGENT)
            .send()
            .await
            .map_err(|e| IndexerError::new(StatusCode::BAD_GATEWAY, e.to_string()))?;

        if !resp.status().is_success() {
            return Err(IndexerError::bad_status("TorrentDay", resp).await);
        }
        check_td_auth(&resp)?;

        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("unknown")
            .to_string();
        let body = resp
            .text()
            .await
            .map_err(|e| IndexerError::new(StatusCode::BAD_GATEWAY, e.to_string()))?;
        let sanitized = fix_invalid_json_escapes(&body);
        let torrents: Vec<TdTorrent> = serde_json::from_str(&sanitized)
            .map_err(|e| IndexerError::json_parse_failed("TorrentDay", &content_type, &body, e))?;

        let mut releases: Vec<Release> = torrents
            .into_iter()
            .map(|t| Self::torrent_to_release(t, base_url))
            .collect();

        for release in &mut releases {
            release.indexer = source.name.clone();
            release.indexer_source_id = Some(source.id);
        }

        Ok(releases)
    }

    async fn test_connection(
        &self,
        ctx: &IndexerContext<'_>,
        config: &serde_json::Value,
    ) -> Result<String, IndexerError> {
        let cookie = config.get("cookie").and_then(|v| v.as_str()).unwrap_or("");
        if cookie.is_empty() {
            return Err(IndexerError::new(
                StatusCode::BAD_REQUEST,
                "cookie is required",
            ));
        }

        let base_url = config
            .get("base_url")
            .and_then(|v| v.as_str())
            .filter(|v| !v.is_empty())
            .unwrap_or(DEFAULT_BASE_URL);

        let resp = ctx
            .http_client
            .get(format!("{}/t.json", base_url.trim_end_matches('/')))
            .header("Cookie", cookie)
            .header("User-Agent", USER_AGENT)
            .send()
            .await
            .map_err(|e| IndexerError::new(StatusCode::BAD_GATEWAY, e.to_string()))?;

        if !resp.status().is_success() {
            return Err(IndexerError::bad_status("TorrentDay", resp).await);
        }
        check_td_auth(&resp)?;

        Ok("Connected to TorrentDay successfully".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_category_mapping() {
        // Movies — all movie categories map to 2xxx range
        for id in [1, 3, 5, 11, 13, 21, 22, 25, 44, 48, 96, 103] {
            let cats = TorrentDayIndexer::map_category(id);
            assert!(
                cats[0] >= 2000 && cats[0] < 3000,
                "TD {id} should be Movies (2xxx), got {}",
                cats[0]
            );
        }
        // TV — all TV categories map to 5xxx range
        for id in [2, 7, 14, 24, 26, 30, 31, 32, 33, 34, 46, 82, 95, 104] {
            let cats = TorrentDayIndexer::map_category(id);
            assert!(
                cats[0] >= 5000 && cats[0] < 6000,
                "TD {id} should be TV (5xxx), got {}",
                cats[0]
            );
        }
        // Games — console (1xxx) and PC (4050)
        for id in [8, 9, 10, 18] {
            let cats = TorrentDayIndexer::map_category(id);
            assert!(
                cats[0] >= 1000 && cats[0] < 2000,
                "TD {id} should be Console Games (1xxx), got {}",
                cats[0]
            );
        }
        for id in [4, 40] {
            let cats = TorrentDayIndexer::map_category(id);
            assert_eq!(cats[0], 4050, "TD {id} should be PC Games (4050)");
        }
        // Music — 3xxx range (not 3030)
        for id in [16, 17, 23, 27, 41, 45] {
            let cats = TorrentDayIndexer::map_category(id);
            assert!(
                cats[0] >= 3000 && cats[0] < 3030 || cats[0] > 3030 && cats[0] < 4000,
                "TD {id} should be Music (3xxx, not 3030), got {}",
                cats[0]
            );
        }
        // Books
        for id in [20, 35, 102] {
            let cats = TorrentDayIndexer::map_category(id);
            assert!(
                cats[0] >= 7000 && cats[0] < 8000,
                "TD {id} should be Books (7xxx), got {}",
                cats[0]
            );
        }
        // Audiobooks
        for id in [42, 94] {
            assert_eq!(
                TorrentDayIndexer::map_category(id),
                vec![3030],
                "TD {id} should be Audiobooks (3030)"
            );
        }
        // Software
        for id in [12, 28, 43] {
            let cats = TorrentDayIndexer::map_category(id);
            assert!(
                cats[0] >= 4000 && cats[0] < 4050,
                "TD {id} should be Software (4000-4049), got {}",
                cats[0]
            );
        }
        // Anime
        assert_eq!(TorrentDayIndexer::map_category(29), vec![5070]);
        // XXX
        for id in [6, 15, 19] {
            let cats = TorrentDayIndexer::map_category(id);
            assert!(
                cats[0] >= 6000 && cats[0] < 7000,
                "TD {id} should be XXX (6xxx), got {}",
                cats[0]
            );
        }
        // Unknown → Other
        assert_eq!(TorrentDayIndexer::map_category(9999), vec![8000]);
        // Fonts → Other
        assert_eq!(TorrentDayIndexer::map_category(47), vec![8000]);
    }

    #[test]
    fn test_newznab_to_td_cats() {
        let books = TorrentDayIndexer::newznab_to_td_cats(&[7020]);
        assert!(books.contains(&35), "books should include TD 35");
        assert!(books.contains(&20), "books should include TD 20");
        assert!(books.contains(&102), "books should include TD 102");

        let movies = TorrentDayIndexer::newznab_to_td_cats(&[2000]);
        assert!(movies.contains(&11), "movies should include TD 11");
        assert!(movies.contains(&48), "movies should include TD 48");
        assert!(movies.contains(&96), "movies should include TD 96");
        assert!(movies.contains(&103), "movies should include TD 103");

        let audiobooks = TorrentDayIndexer::newznab_to_td_cats(&[3030]);
        assert!(audiobooks.contains(&42), "audiobooks should include TD 42");
        assert!(audiobooks.contains(&94), "audiobooks should include TD 94");

        let tv = TorrentDayIndexer::newznab_to_td_cats(&[5000]);
        assert!(tv.contains(&7), "tv should include TD 7");
        assert!(tv.contains(&34), "tv should include TD 34");
        assert!(tv.contains(&104), "tv should include TD 104");

        let software = TorrentDayIndexer::newznab_to_td_cats(&[4000]);
        assert!(software.contains(&12), "software should include TD 12");
        assert!(software.contains(&28), "software should include TD 28");
        assert!(software.contains(&43), "software should include TD 43");

        let xxx = TorrentDayIndexer::newznab_to_td_cats(&[6000]);
        assert!(xxx.contains(&6), "xxx should include TD 6");
        assert!(xxx.contains(&15), "xxx should include TD 15");
        assert!(xxx.contains(&19), "xxx should include TD 19");
    }

    /// Every mapped TD category must roundtrip through newznab_to_td_cats.
    #[test]
    fn test_newznab_to_td_roundtrip() {
        let all_td_ids = [
            1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24,
            25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 40, 41, 42, 43, 44, 45, 46, 48, 82, 94, 95,
            96, 102, 103, 104,
        ];
        for td_id in all_td_ids {
            let newznab = TorrentDayIndexer::map_category(td_id);
            assert_ne!(newznab, vec![8000], "TD {td_id} should not map to Other");
            let reverse = TorrentDayIndexer::newznab_to_td_cats(&newznab);
            assert!(
                reverse.contains(&td_id),
                "TD {td_id} -> newznab {:?} -> reverse {:?} missing {td_id}",
                newznab,
                reverse
            );
        }
    }

    #[test]
    fn test_torrent_to_release() {
        let torrent = TdTorrent {
            id: 54321,
            name: "Test.Movie.2024.1080p".to_string(),
            size: 2_000_000_000,
            seeders: 50,
            leechers: 5,
            category_id: 11,
            ctime: 1704067200, // 2024-01-01 00:00:00 UTC
            files: Some(3),
            completed: None,
            imdb_id: Some("1264573".to_string()),
            download_multiplier: Some(0.0),
        };

        let release = TorrentDayIndexer::torrent_to_release(torrent, DEFAULT_BASE_URL);
        assert_eq!(release.guid, "td-54321");
        assert_eq!(release.title, "Test.Movie.2024.1080p");
        assert!(release.download_url.unwrap().contains("54321"));
        assert_eq!(release.category_ids, vec![2050]);
        assert!(!release.publish_date.is_empty());
        // IMDB ID extracted and normalized to tt-prefixed 7-digit format
        assert_eq!(release.external_ids.len(), 1);
        assert_eq!(release.external_ids[0].id, "tt1264573");
        // download_multiplier stored in raw_data
        assert_eq!(release.raw_data["download_multiplier"], 0.0);
    }

    #[test]
    fn test_config_freeleech_string_false() {
        let json = serde_json::json!({
            "cookie": "uid=1; pass=abc",
            "freeleech_only": "false"
        });
        let config: TorrentDayConfig = serde_json::from_value(json).unwrap();
        assert!(!config.freeleech_only);
    }

    #[test]
    fn test_config_freeleech_string_true() {
        let json = serde_json::json!({
            "cookie": "uid=1; pass=abc",
            "freeleech_only": "true"
        });
        let config: TorrentDayConfig = serde_json::from_value(json).unwrap();
        assert!(config.freeleech_only);
    }

    #[test]
    fn test_config_schema() {
        let indexer = TorrentDayIndexer;
        let schema = indexer.config_schema();
        let fields = schema.get("fields").unwrap().as_array().unwrap();
        assert_eq!(fields.len(), 3);
        assert_eq!(fields[0]["name"], "cookie");
    }

    #[test]
    fn test_deserialize_response() {
        let json = serde_json::json!([
            {
                "t": 100,
                "name": "Test Torrent",
                "size": 1024,
                "seeders": 10,
                "leechers": 2,
                "c": 35,
                "ctime": 1704067200,
                "category": "Books/Ebooks",
                "files": 1,
                "completed": 5
            }
        ]);

        let torrents: Vec<TdTorrent> = serde_json::from_value(json).unwrap();
        assert_eq!(torrents.len(), 1);
        assert_eq!(torrents[0].id, 100);
        assert_eq!(torrents[0].name, "Test Torrent");
        assert_eq!(torrents[0].category_id, 35);
        assert_eq!(torrents[0].ctime, 1704067200);
    }

    /// Real response format from TorrentDay as of 2026-03 — includes extra
    /// fields like `category` (string), `imdb-id`, and `download-multiplier`
    /// that the struct must tolerate via `#[serde(deny_unknown_fields)]` NOT
    /// being set.
    #[test]
    fn test_deserialize_real_response() {
        let raw = r#"[
            {"t":9605699,"c":27,"size":533127569,"files":15,"seeders":3,"leechers":1,"completed":1,"ctime":1772986563,"category":"Music/Flac","name":"VA-Talla 2XLC-FLAC-2026-STAX"},
            {"t":9605696,"c":11,"size":31590183330,"files":1,"seeders":3,"leechers":1,"completed":0,"ctime":1772986237,"category":"Movies/Bluray","name":"Lurker (2025) 1080p BluRay","download-multiplier":0,"imdb-id":"1264573"},
            {"t":9605695,"c":34,"size":1361379998,"files":5,"seeders":46,"leechers":5,"completed":58,"ctime":1772985891,"category":"TV/x265","name":"Saturday Night Live S51E14","imdb-id":"1667"}
        ]"#;

        let torrents: Vec<TdTorrent> = serde_json::from_str(raw).unwrap();
        assert_eq!(torrents.len(), 3);
        assert_eq!(torrents[0].id, 9605699);
        assert_eq!(torrents[0].category_id, 27);
        assert_eq!(torrents[1].id, 9605696);
        assert_eq!(torrents[1].category_id, 11);
        assert_eq!(torrents[2].name, "Saturday Night Live S51E14");
    }

    /// Verify that the shared HTTP client can decompress gzip responses.
    /// TorrentDay returns `content-encoding: gzip` — without this, reqwest
    /// tries to parse raw gzip bytes as JSON and fails with
    /// "error decoding response body".
    #[tokio::test]
    async fn test_gzip_response_decompression() {
        use axum::http::header;
        use axum::routing::get;

        let json_body = r#"[{"t":100,"c":35,"size":1024,"seeders":10,"leechers":2,"completed":5,"ctime":1704067200,"category":"Books/Ebooks","name":"Test Torrent","files":1}]"#;

        // Gzip-compress the JSON body
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut encoder, json_body.as_bytes()).unwrap();
        let compressed = encoder.finish().unwrap();

        let app = axum::Router::new().route(
            "/t.json",
            get(move || {
                let body = compressed.clone();
                async move {
                    (
                        [
                            (header::CONTENT_TYPE, "application/json"),
                            (header::CONTENT_ENCODING, "gzip"),
                        ],
                        body,
                    )
                }
            }),
        );

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(axum::serve(listener, app).into_future());

        let client = crate::http::new_client();
        let resp = client
            .get(format!("http://{addr}/t.json"))
            .send()
            .await
            .unwrap();

        let torrents: Vec<TdTorrent> = resp.json().await.expect(
            "gzip decompression must work — reqwest needs the `gzip` feature + .gzip(true)",
        );
        assert_eq!(torrents.len(), 1);
        assert_eq!(torrents[0].id, 100);
        assert_eq!(torrents[0].name, "Test Torrent");
    }

    #[test]
    fn test_fix_invalid_json_escapes() {
        // Valid escapes pass through unchanged
        let valid = r#"{"name":"foo\\bar","path":"c:\\dir"}"#;
        assert_eq!(fix_invalid_json_escapes(valid), valid);

        // Invalid escape \P gets doubled
        let bad = r#"[{"name":"test\Pack"}]"#;
        let fixed = fix_invalid_json_escapes(bad);
        assert_eq!(fixed, r#"[{"name":"test\\Pack"}]"#);
        let parsed: Vec<serde_json::Value> = serde_json::from_str(&fixed).unwrap();
        assert_eq!(parsed[0]["name"], "test\\Pack");

        // Multiple invalid escapes in one string
        let bad2 = r#"{"a":"x\yz\qw"}"#;
        let fixed2 = fix_invalid_json_escapes(bad2);
        let parsed2: serde_json::Value = serde_json::from_str(&fixed2).unwrap();
        assert_eq!(parsed2["a"], "x\\yz\\qw");

        // Valid \n \t \u escapes preserved
        let with_valid = r#"{"a":"line\none\ttwo\u0041"}"#;
        assert_eq!(fix_invalid_json_escapes(with_valid), with_valid);

        // No strings — no changes
        assert_eq!(fix_invalid_json_escapes("[1,2,3]"), "[1,2,3]");
    }
}
