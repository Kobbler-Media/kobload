use axum::http::StatusCode;
use serde::Deserialize;
use time::OffsetDateTime;
use tracing::{info, warn};

use crate::repositories::watch as watch_repo;
use crate::watch::Release;

use super::types::{
    IndexerCapabilities, IndexerContext, IndexerError, IndexerHandler, IndexerSearchRequest,
};

const MAM_BASE: &str = "https://www.myanonamouse.net";

/// MAM search_type parameter values.
mod search_type {
    pub const ALL: &str = "all";
}

/// Freeleech wedge usage policy.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum FreeleechWedgePolicy {
    #[default]
    Never,
    Preferred,
    Required,
}

impl FreeleechWedgePolicy {
    fn from_str(s: &str) -> Self {
        match s {
            "preferred" => Self::Preferred,
            "required" => Self::Required,
            _ => Self::Never,
        }
    }
}

#[derive(Debug, Deserialize)]
struct MamConfig {
    mam_id: String,
    #[serde(default = "default_search_type")]
    search_type: String,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_bool_lenient"
    )]
    search_in_description: bool,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_bool_lenient"
    )]
    search_in_series: bool,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_bool_lenient"
    )]
    search_in_filenames: bool,
    #[serde(default = "default_wedge_policy")]
    use_freeleech_wedge: String,
    /// Read by SQL: `handler_config->>'poll_interval_minutes'` in get_sources_to_check.
    #[serde(
        default = "default_poll_interval",
        deserialize_with = "super::serde_helpers::deserialize_u32_lenient"
    )]
    #[allow(dead_code)]
    poll_interval_minutes: u32,
}

fn default_poll_interval() -> u32 {
    30
}

fn default_search_type() -> String {
    search_type::ALL.to_string()
}

fn default_wedge_policy() -> String {
    "never".to_string()
}

#[derive(Debug, Deserialize)]
struct MamSearchResponse {
    #[serde(default)]
    data: Vec<MamTorrent>,
}

#[derive(Debug, Deserialize)]
struct MamTorrent {
    id: i64,
    title: String,
    #[serde(default)]
    author_info: String,
    #[serde(default, deserialize_with = "deserialize_string_or_int")]
    category: String,
    #[serde(default, deserialize_with = "deserialize_string_or_int")]
    main_cat: String,
    #[serde(default)]
    leechers: i32,
    #[serde(default)]
    seeders: i32,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_bool_lenient"
    )]
    vip: bool,
    #[serde(
        default,
        deserialize_with = "super::serde_helpers::deserialize_bool_lenient"
    )]
    free: bool,
    #[serde(default, deserialize_with = "deserialize_size")]
    size: i64,
    #[serde(default)]
    added: String,
    #[serde(default, alias = "numfiles")]
    num_files: Option<i32>,
    #[serde(default)]
    filetype: String,
}

fn deserialize_string_or_int<'de, D: serde::Deserializer<'de>>(d: D) -> Result<String, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum StringOrInt {
        Str(String),
        Int(i64),
    }
    match StringOrInt::deserialize(d)? {
        StringOrInt::Str(s) => Ok(s),
        StringOrInt::Int(v) => Ok(v.to_string()),
    }
}

fn parse_size_with_unit(s: &str) -> Option<i64> {
    let s = s.trim();
    // Try splitting on last space: "113.6 MiB" or no space: "113.6MiB"
    let pos = s.rfind(|c: char| c.is_ascii_digit() || c == '.')?;
    let (n, u) = s.split_at(pos + 1);
    let (num_str, unit) = (n.trim(), u.trim());
    if unit.is_empty() {
        return None;
    }
    let num: f64 = num_str.parse().ok()?;
    let multiplier: f64 = match unit.to_ascii_lowercase().as_str() {
        "b" => 1.0,
        "kib" | "kb" => 1024.0,
        "mib" | "mb" => 1024.0 * 1024.0,
        "gib" | "gb" => 1024.0 * 1024.0 * 1024.0,
        "tib" | "tb" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
        _ => return None,
    };
    Some((num * multiplier) as i64)
}

fn deserialize_size<'de, D: serde::Deserializer<'de>>(d: D) -> Result<i64, D::Error> {
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum SizeValue {
        Null(()),
        Int(i64),
        Float(f64),
        Str(String),
    }
    match SizeValue::deserialize(d)? {
        SizeValue::Null(()) => Ok(0),
        SizeValue::Int(v) => Ok(v),
        SizeValue::Float(v) => Ok(v as i64),
        SizeValue::Str(s) => {
            // Try human-readable format first ("113.6 MiB"), then plain number
            if let Some(bytes) = parse_size_with_unit(&s) {
                return Ok(bytes);
            }
            Ok(s.parse::<i64>()
                .or_else(|_| s.parse::<f64>().map(|v| v as i64))
                .unwrap_or(0))
        }
    }
}

#[derive(Debug, Deserialize)]
struct MamUserInfo {
    #[serde(default)]
    classname: String,
    #[serde(default, rename = "personalFL")]
    #[allow(dead_code)]
    personal_fl: Option<i32>,
}

impl MamUserInfo {
    fn is_vip(&self) -> bool {
        self.classname == "VIP"
    }
}

/// Cached VIP status in source.state
#[derive(Debug, Deserialize, serde::Serialize)]
struct MamCachedState {
    is_vip: bool,
    vip_checked_at: String,
}

pub struct MamIndexer;

impl MamIndexer {
    fn build_cookie(mam_id: &str) -> String {
        format!("mam_id={mam_id}")
    }

    /// Map MAM category string to newznab-compatible category IDs.
    fn map_category(category: &str, main_cat: &str) -> Vec<i32> {
        // MAM main categories: Ebooks, Audiobooks, Musicology, Radio, etc.
        match main_cat {
            "Ebooks" | "14" => match category {
                // Ebook subcategories
                "epub" | "EPUB" | "61" => vec![7020],
                "pdf" | "PDF" | "62" => vec![7020],
                "mobi" | "MOBI" | "63" => vec![7020],
                "azw" | "AZW" | "64" => vec![7020],
                "Comics" | "80" => vec![7030],
                "Manga" | "83" => vec![7030],
                "Magazines" | "84" => vec![7010],
                "Newspapers" | "85" => vec![7010],
                "Technical/Academic" | "79" => vec![7020],
                _ => vec![7020], // Default ebooks
            },
            "Audiobooks" | "13" => vec![3030],
            "Musicology" | "15" => vec![3030],
            "Radio" | "16" => vec![3030],
            _ => vec![7000], // Fallback to Books
        }
    }

    fn torrent_to_release(t: MamTorrent) -> Release {
        let download_url = format!("{}/tor/download.php?tid={}", MAM_BASE, t.id);
        let info_url = format!("{}/t/{}", MAM_BASE, t.id);
        let category_ids = Self::map_category(&t.category, &t.main_cat);

        let mut title = t.title.clone();
        if !t.author_info.is_empty() {
            let author = parse_author_info(&t.author_info);
            if !author.is_empty() {
                title = format!("{} - {}", author, title);
            }
        }

        let raw_data = serde_json::json!({
            "mam_id": t.id,
            "category": t.category,
            "main_cat": t.main_cat,
            "vip": t.vip,
            "free": t.free,
            "filetype": t.filetype,
            "author_info": t.author_info,
        });

        Release {
            guid: format!("mam-{}", t.id),
            title,
            file_name: None,
            download_url: Some(download_url),
            magnet_url: None,
            info_url: Some(info_url),
            size: t.size,
            publish_date: t.added,
            category_ids,
            indexer: String::new(), // Set by caller
            protocol: "torrent".to_string(),
            info_hash: None,
            files: t.num_files,
            seeders: Some(t.seeders),
            peers: Some(t.seeders + t.leechers),
            leechers: Some(t.leechers),
            grabs: None,
            votes: None,
            indexer_id: Some(t.id as i32),
            indexer_source_id: None,
            raw_data,
            external_ids: Vec::new(),
            media: None,
        }
    }

    async fn check_vip(
        ctx: &IndexerContext<'_>,
        source: &watch_repo::IndexerSourceRow,
        mam_id: &str,
    ) -> bool {
        // Check cached state first (24h TTL)
        if let Some(ref state) = source.state
            && let Ok(cached) = serde_json::from_value::<MamCachedState>(state.clone())
            && let Ok(checked_at) = OffsetDateTime::parse(
                &cached.vip_checked_at,
                &time::format_description::well_known::Rfc3339,
            )
        {
            let age = OffsetDateTime::now_utc() - checked_at;
            if age < time::Duration::hours(24) {
                return cached.is_vip;
            }
        }

        // Fetch from API
        let url = format!("{}/jsonLoad.php", MAM_BASE);
        let resp = ctx
            .http_client
            .get(&url)
            .header("Cookie", Self::build_cookie(mam_id))
            .send()
            .await;

        let is_vip = match resp {
            Ok(r) if r.status().is_success() => r
                .json::<MamUserInfo>()
                .await
                .map(|u| u.is_vip())
                .unwrap_or(false),
            _ => false,
        };

        // Cache result — merge into existing state to preserve RSS watermark
        let now = OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)
            .unwrap_or_default();
        let mut state = source
            .state
            .clone()
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();
        state.insert("is_vip".to_string(), serde_json::json!(is_vip));
        state.insert("vip_checked_at".to_string(), serde_json::json!(now));
        let state = serde_json::Value::Object(state);
        let _ =
            watch_repo::update_indexer_source_state(ctx.pool, source.id, Some(&state), None).await;

        is_vip
    }
}

/// Parse MAM author_info field. The API returns a JSON object like
/// `{"34767":"Educational Testing Service"}` where keys are IDs and values are names.
/// Falls back to HTML-stripping for non-JSON strings.
fn parse_author_info(s: &str) -> String {
    if let Ok(map) = serde_json::from_str::<std::collections::HashMap<String, String>>(s) {
        let names: Vec<&str> = map
            .values()
            .map(|v| v.as_str())
            .filter(|v| !v.is_empty())
            .collect();
        if !names.is_empty() {
            return names.join(", ");
        }
        return String::new();
    }
    // Fallback: strip HTML tags (legacy format)
    let author = s.replace("<br/>", ", ").replace("<br>", ", ");
    strip_html_tags(&author)
}

fn strip_html_tags(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    let mut in_tag = false;
    for ch in s.chars() {
        match ch {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => result.push(ch),
            _ => {}
        }
    }
    result.trim().to_string()
}

#[async_trait::async_trait]
impl IndexerHandler for MamIndexer {
    fn handler_type(&self) -> &'static str {
        "mam"
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
                    "name": "mam_id",
                    "label": "MAM ID Cookie",
                    "type": "password",
                    "required": true,
                    "placeholder": "Your mam_id cookie value",
                    "help": "On MyAnonaMouse: Preferences > Security > create a new session, \
        lock it to this server's IP or ASN, then paste the mam_id value. It is shown only once, and a \
        session locked to an IP stops working when that IP changes.",
                    "help_url": "https://www.myanonamouse.net/preferences/index.php?view=security"
                },
                {
                    "name": "search_type",
                    "label": "Search Type",
                    "type": "select",
                    "default": "all",
                    "options": {
                        "all": "All torrents",
                        "active": "Active only",
                        "fl": "Freeleech",
                        "fl-VIP": "Freeleech + VIP",
                        "VIP": "VIP only",
                        "nVIP": "Non-VIP only"
                    }
                },
                {
                    "name": "search_in_description",
                    "label": "Search in description",
                    "type": "checkbox",
                    "default": false
                },
                {
                    "name": "search_in_series",
                    "label": "Search in series",
                    "type": "checkbox",
                    "default": false
                },
                {
                    "name": "search_in_filenames",
                    "label": "Search in filenames",
                    "type": "checkbox",
                    "default": false
                },
                {
                    "name": "use_freeleech_wedge",
                    "label": "Use freeleech wedge",
                    "type": "select",
                    "default": "never",
                    "options": {
                        "never": "Never",
                        "preferred": "Preferred (use if available)",
                        "required": "Required (skip non-FL if no wedges)"
                    }
                },
                {
                    "name": "poll_interval_minutes",
                    "label": "RSS Poll Interval (minutes)",
                    "type": "number",
                    "default": 30,
                    "min": 15,
                    "max": 1440
                }
            ],
            "default_media_type_ids": ["ebooks", "audiobooks"]
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

        let config: MamConfig = serde_json::from_value(source.handler_config.clone())
            .map_err(|e| IndexerError::new(StatusCode::BAD_REQUEST, e.to_string()))?;

        if config.mam_id.trim().is_empty() {
            return Err(IndexerError::new(
                StatusCode::BAD_REQUEST,
                "mam_id not configured",
            ));
        }

        let cookie = Self::build_cookie(&config.mam_id);
        let url = format!("{}/tor/js/loadSearchJSONbasic.php", MAM_BASE);

        let mut params: Vec<(String, String)> = Vec::new();
        params.push(("tor[text]".to_string(), request.query.clone()));
        params.push(("tor[srchIn][title]".to_string(), "true".to_string()));
        if config.search_in_description {
            params.push(("tor[srchIn][description]".to_string(), "true".to_string()));
        }
        if config.search_in_series {
            params.push(("tor[srchIn][series]".to_string(), "true".to_string()));
        }
        if config.search_in_filenames {
            params.push(("tor[srchIn][filenames]".to_string(), "true".to_string()));
        }
        params.push(("tor[searchType]".to_string(), config.search_type.clone()));

        // Map newznab category IDs to MAM main categories
        if !request.categories.is_empty() {
            for cat_id in &request.categories {
                let mam_cats = match cat_id {
                    7000..=7999 => Some("14"), // Ebooks
                    3030 => Some("13"),        // Audiobooks
                    _ => None,
                };
                if let Some(cat) = mam_cats {
                    params.push(("tor[main_cat][]".to_string(), cat.to_string()));
                }
            }
        }

        params.push(("tor[searchIn]".to_string(), "torrents".to_string()));
        params.push((
            "tor[perpage]".to_string(),
            request.limit.unwrap_or(100).to_string(),
        ));

        let resp = ctx
            .http_client
            .get(&url)
            .header("Cookie", &cookie)
            .query(&params)
            .send()
            .await
            .map_err(|e| IndexerError::new(StatusCode::BAD_GATEWAY, e.to_string()))?;

        if !resp.status().is_success() {
            return Err(IndexerError::bad_status("MAM", resp).await);
        }

        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("unknown")
            .to_string();
        let text = resp
            .text()
            .await
            .map_err(|e| IndexerError::new(StatusCode::BAD_GATEWAY, e.to_string()))?;
        let body: MamSearchResponse = serde_json::from_str(&text)
            .map_err(|e| IndexerError::json_parse_failed("MAM", &content_type, &text, e))?;

        let mut releases: Vec<Release> = body
            .data
            .into_iter()
            .map(MamIndexer::torrent_to_release)
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
        let config: MamConfig = serde_json::from_value(source.handler_config.clone())
            .map_err(|e| IndexerError::new(StatusCode::BAD_REQUEST, e.to_string()))?;

        if config.mam_id.trim().is_empty() {
            return Err(IndexerError::new(
                StatusCode::BAD_REQUEST,
                "mam_id not configured",
            ));
        }

        let cookie = Self::build_cookie(&config.mam_id);
        let url = format!("{}/tor/js/loadSearchJSONbasic.php", MAM_BASE);

        // Browse recent uploads across both Ebooks and Audiobooks, sorted newest first
        let params: Vec<(&str, &str)> = vec![
            ("tor[text]", ""),
            ("tor[searchType]", &config.search_type),
            ("tor[searchIn]", "torrents"),
            ("tor[perpage]", "100"),
            ("tor[main_cat][]", "14"), // Ebooks
            ("tor[main_cat][]", "13"), // Audiobooks
            ("tor[sortType]", "dateDesc"),
        ];

        let resp = ctx
            .http_client
            .get(&url)
            .header("Cookie", &cookie)
            .query(&params)
            .send()
            .await
            .map_err(|e| IndexerError::new(StatusCode::BAD_GATEWAY, e.to_string()))?;

        if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let retry_after = resp
                .headers()
                .get("retry-after")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok());
            return Err(IndexerError::rate_limited(retry_after));
        }

        if !resp.status().is_success() {
            return Err(IndexerError::bad_status("MAM", resp).await);
        }

        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("unknown")
            .to_string();
        let text = resp
            .text()
            .await
            .map_err(|e| IndexerError::new(StatusCode::BAD_GATEWAY, e.to_string()))?;
        let body: MamSearchResponse = serde_json::from_str(&text)
            .map_err(|e| IndexerError::json_parse_failed("MAM", &content_type, &text, e))?;

        let mut releases: Vec<Release> = body
            .data
            .into_iter()
            .map(MamIndexer::torrent_to_release)
            .collect();

        for release in &mut releases {
            release.indexer = source.name.clone();
            release.indexer_source_id = Some(source.id);
        }

        Ok(releases)
    }

    async fn prepare_download(
        &self,
        ctx: &IndexerContext<'_>,
        source: &watch_repo::IndexerSourceRow,
        download_url: &str,
    ) -> Result<String, IndexerError> {
        let config: MamConfig = serde_json::from_value(source.handler_config.clone())
            .map_err(|e| IndexerError::new(StatusCode::BAD_REQUEST, e.to_string()))?;

        let policy = FreeleechWedgePolicy::from_str(&config.use_freeleech_wedge);
        if policy == FreeleechWedgePolicy::Never {
            return Ok(download_url.to_string());
        }

        // Extract torrent ID from URL
        let tid = download_url
            .split("tid=")
            .nth(1)
            .and_then(|s| s.split('&').next())
            .and_then(|s| s.parse::<i64>().ok());

        let Some(tid) = tid else {
            return Ok(download_url.to_string());
        };

        // Check if user is VIP (FL is free for VIPs)
        let is_vip = Self::check_vip(ctx, source, &config.mam_id).await;
        if is_vip {
            info!(tid, "MAM VIP user, freeleech is automatic");
            return Ok(download_url.to_string());
        }

        // Try to buy a personal freeleech wedge
        let cookie = Self::build_cookie(&config.mam_id);
        let buy_url = format!("{}/json/bonusBuy.php", MAM_BASE);
        let resp = ctx
            .http_client
            .get(&buy_url)
            .header("Cookie", &cookie)
            .query(&[("spendtype", "personalFL"), ("torrentid", &tid.to_string())])
            .send()
            .await;

        match resp {
            Ok(r) if r.status().is_success() => {
                let body = r.text().await.unwrap_or_default();
                if body.contains("\"success\":true") || body.contains("\"success\": true") {
                    info!(tid, "MAM personal FL wedge purchased");
                } else {
                    let msg = format!("MAM FL wedge purchase failed: {}", body);
                    if policy == FreeleechWedgePolicy::Required {
                        return Err(IndexerError::new(StatusCode::PAYMENT_REQUIRED, msg));
                    }
                    warn!(tid, "{}", msg);
                }
            }
            Ok(r) => {
                let msg = format!("MAM FL wedge API returned {}", r.status());
                if policy == FreeleechWedgePolicy::Required {
                    return Err(IndexerError::new(StatusCode::BAD_GATEWAY, msg));
                }
                warn!(tid, "{}", msg);
            }
            Err(e) => {
                let msg = format!("MAM FL wedge request failed: {}", e);
                if policy == FreeleechWedgePolicy::Required {
                    return Err(IndexerError::new(StatusCode::BAD_GATEWAY, msg));
                }
                warn!(tid, "{}", msg);
            }
        }

        Ok(download_url.to_string())
    }

    async fn test_connection(
        &self,
        ctx: &IndexerContext<'_>,
        config: &serde_json::Value,
    ) -> Result<String, IndexerError> {
        let mam_id = config.get("mam_id").and_then(|v| v.as_str()).unwrap_or("");
        if mam_id.is_empty() {
            return Err(IndexerError::new(
                StatusCode::BAD_REQUEST,
                "mam_id is required",
            ));
        }

        let resp = ctx
            .http_client
            .get(format!("{MAM_BASE}/jsonLoad.php"))
            .header("Cookie", format!("mam_id={mam_id}"))
            .send()
            .await
            .map_err(|e| IndexerError::new(StatusCode::BAD_GATEWAY, e.to_string()))?;

        if !resp.status().is_success() {
            return Err(IndexerError::bad_status("MAM", resp).await);
        }

        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("unknown")
            .to_string();
        let text = resp
            .text()
            .await
            .map_err(|e| IndexerError::new(StatusCode::BAD_GATEWAY, e.to_string()))?;
        let body: serde_json::Value = serde_json::from_str(&text)
            .map_err(|e| IndexerError::json_parse_failed("MAM", &content_type, &text, e))?;

        let username = body
            .get("username")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown");

        Ok(format!("Connected to MAM as {username}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_category_mapping() {
        assert_eq!(MamIndexer::map_category("epub", "Ebooks"), vec![7020]);
        assert_eq!(MamIndexer::map_category("Comics", "Ebooks"), vec![7030]);
        assert_eq!(MamIndexer::map_category("any", "Audiobooks"), vec![3030]);
        assert_eq!(MamIndexer::map_category("any", "Unknown"), vec![7000]);
    }

    #[test]
    fn test_strip_html_tags() {
        assert_eq!(strip_html_tags("<b>Author</b>"), "Author");
        assert_eq!(strip_html_tags("Author1<br/>Author2"), "Author1Author2");
        assert_eq!(strip_html_tags("plain text"), "plain text");
    }

    #[test]
    fn test_torrent_to_release() {
        let torrent = MamTorrent {
            id: 12345,
            title: "Test Book".to_string(),
            author_info: "John Doe".to_string(),
            category: "epub".to_string(),
            main_cat: "Ebooks".to_string(),
            leechers: 2,
            seeders: 10,
            vip: false,
            free: false,
            size: 1048576,
            added: "2024-01-01 00:00:00".to_string(),
            num_files: Some(1),
            filetype: "epub".to_string(),
        };

        let release = MamIndexer::torrent_to_release(torrent);
        assert_eq!(release.guid, "mam-12345");
        assert_eq!(release.title, "John Doe - Test Book");
        assert_eq!(release.size, 1048576);
        assert_eq!(release.seeders, Some(10));
        assert_eq!(release.leechers, Some(2));
        assert_eq!(release.protocol, "torrent");
        assert!(release.download_url.unwrap().contains("tid=12345"));
        assert_eq!(release.category_ids, vec![7020]);
    }

    #[test]
    fn test_config_schema() {
        let indexer = MamIndexer;
        let schema = indexer.config_schema();
        let fields = schema.get("fields").unwrap().as_array().unwrap();
        assert!(fields.len() >= 7);
        assert_eq!(fields[0]["name"], "mam_id");

        // poll_interval_minutes field exists
        let poll_field = fields
            .iter()
            .find(|f| f["name"] == "poll_interval_minutes")
            .expect("poll_interval_minutes field missing");
        assert_eq!(poll_field["default"], 30);
    }

    #[test]
    fn test_capabilities_supports_rss() {
        let indexer = MamIndexer;
        let caps = indexer.capabilities();
        assert!(caps.supports_search);
        assert!(caps.supports_rss);
    }

    #[test]
    fn test_freeleech_policy() {
        assert_eq!(
            FreeleechWedgePolicy::from_str("never"),
            FreeleechWedgePolicy::Never
        );
        assert_eq!(
            FreeleechWedgePolicy::from_str("preferred"),
            FreeleechWedgePolicy::Preferred
        );
        assert_eq!(
            FreeleechWedgePolicy::from_str("required"),
            FreeleechWedgePolicy::Required
        );
        assert_eq!(
            FreeleechWedgePolicy::from_str("unknown"),
            FreeleechWedgePolicy::Never
        );
    }

    #[test]
    fn test_deserialize_size_float_string() {
        let json = serde_json::json!({
            "data": [{
                "id": 100,
                "title": "Float Size Book",
                "author_info": "",
                "category": "46",
                "main_cat": "13",
                "leechers": 0,
                "seeders": 1,
                "vip": false,
                "free": false,
                "size": "126.05",
                "added": "2026-03-24 00:00:00",
                "num_files": 1,
                "filetype": "m4b"
            }]
        });

        let resp: MamSearchResponse = serde_json::from_value(json).unwrap();
        assert_eq!(resp.data[0].size, 126);
    }

    #[test]
    fn test_deserialize_size_float_number() {
        let json = serde_json::json!({
            "data": [{
                "id": 101,
                "title": "Float Number Size",
                "author_info": "",
                "category": "46",
                "main_cat": "13",
                "leechers": 0,
                "seeders": 1,
                "vip": false,
                "free": false,
                "size": 1048576.5,
                "added": "2026-03-24 00:00:00",
                "num_files": 1,
                "filetype": "m4b"
            }]
        });

        let resp: MamSearchResponse = serde_json::from_value(json).unwrap();
        assert_eq!(resp.data[0].size, 1048576);
    }

    #[test]
    fn test_deserialize_search_response() {
        let json = serde_json::json!({
            "data": [
                {
                    "id": 999,
                    "title": "A Book",
                    "author_info": "",
                    "category": "epub",
                    "main_cat": "Ebooks",
                    "leechers": 0,
                    "seeders": 5,
                    "vip": false,
                    "free": true,
                    "size": "2048",
                    "added": "2024-06-15 12:00:00",
                    "num_files": 1,
                    "filetype": "epub"
                }
            ]
        });

        let resp: MamSearchResponse = serde_json::from_value(json).unwrap();
        assert_eq!(resp.data.len(), 1);
        assert_eq!(resp.data[0].id, 999);
        assert_eq!(resp.data[0].size, 2048); // String -> i64 deserialization
        assert!(resp.data[0].free);
    }

    #[test]
    fn test_deserialize_size_empty_string() {
        let json = serde_json::json!({
            "data": [{
                "id": 102,
                "title": "Empty Size",
                "author_info": "",
                "category": "46",
                "main_cat": "13",
                "leechers": 0,
                "seeders": 1,
                "vip": false,
                "free": false,
                "size": "",
                "added": "2026-03-24 00:00:00",
                "num_files": 1,
                "filetype": "epub"
            }]
        });

        let resp: MamSearchResponse = serde_json::from_value(json).unwrap();
        assert_eq!(resp.data[0].size, 0);
    }

    #[test]
    fn test_deserialize_size_null() {
        let json = serde_json::json!({
            "data": [{
                "id": 103,
                "title": "Null Size",
                "author_info": "",
                "category": "46",
                "main_cat": "13",
                "leechers": 0,
                "seeders": 1,
                "vip": false,
                "free": false,
                "size": null,
                "added": "2026-03-24 00:00:00",
                "num_files": 1,
                "filetype": "epub"
            }]
        });

        let resp: MamSearchResponse = serde_json::from_value(json).unwrap();
        assert_eq!(resp.data[0].size, 0);
    }

    #[test]
    fn test_deserialize_real_api_response() {
        let json = serde_json::json!({
            "perpage": 20,
            "start": 0,
            "data": [{
                "id": 747262,
                "language": 1,
                "lang_code": "ENG",
                "main_cat": 14,
                "category": 78,
                "mediatype": 2,
                "maincat": 0,
                "categories": "[24]",
                "catname": "Ebooks - Language",
                "size": "113.6 MiB",
                "numfiles": 101,
                "vip": 0,
                "vip_expire": 0,
                "free": 0,
                "personal_freeleech": 0,
                "fl_vip": 0,
                "title": "The Official Guide to the TOEFLiBT Test 6th",
                "w": 1640,
                "tags": "Publication Date: 25-08-2020, PDF plus Test Audio Files",
                "author_info": "{\"34767\":\"Educational Testing Service\"}",
                "narrator_info": "{}",
                "series_info": "{}",
                "filetype": "mp3 pdf",
                "seeders": 10,
                "leechers": 0,
                "added": "2021-08-20 18:28:06",
                "browseflags": 0,
                "times_completed": 50,
                "comments": 0,
                "bookmarked": null,
                "my_snatched": 0,
                "poster_type": "image/jpeg",
                "mediainfo": "{}",
                "ownership": "[]",
                "cat": "<div class=\"cat78\">&nbsp;</div>"
            }]
        });

        let resp: MamSearchResponse = serde_json::from_value(json).unwrap();
        assert_eq!(resp.data.len(), 1);
        let t = &resp.data[0];
        assert_eq!(t.id, 747262);
        assert_eq!(t.title, "The Official Guide to the TOEFLiBT Test 6th");
        assert!(!t.vip);
        assert!(!t.free);
        assert_eq!(t.num_files, Some(101));
        assert_eq!(t.seeders, 10);
        assert_eq!(t.leechers, 0);
        assert_eq!(t.category, "78");
        assert_eq!(t.main_cat, "14");
        assert_eq!(t.filetype, "mp3 pdf");
        // 113.6 MiB = 113.6 * 1024 * 1024 = 119_118_028 bytes (approx)
        assert!(
            t.size > 119_000_000 && t.size < 119_200_000,
            "size was {}",
            t.size
        );

        // Verify torrent_to_release parses author_info JSON
        let release = MamIndexer::torrent_to_release(MamTorrent {
            id: t.id,
            title: t.title.clone(),
            author_info: "{\"34767\":\"Educational Testing Service\"}".to_string(),
            category: t.category.clone(),
            main_cat: t.main_cat.clone(),
            leechers: t.leechers,
            seeders: t.seeders,
            vip: t.vip,
            free: t.free,
            size: t.size,
            added: t.added.clone(),
            num_files: t.num_files,
            filetype: t.filetype.clone(),
        });
        assert!(release.title.contains("Educational Testing Service"));
        assert!(release.title.contains("The Official Guide"));
    }

    #[test]
    fn test_deserialize_size_with_units() {
        let cases = [
            ("113.6 MiB", 119_118_028),
            ("236.2 MiB", 247_659_724),
            ("1.2 GiB", 1_288_490_188),
            ("500 KiB", 512_000),
            ("1 TiB", 1_099_511_627_776),
        ];
        for (input, expected) in cases {
            let json = serde_json::json!({
                "data": [{
                    "id": 1,
                    "title": "T",
                    "author_info": "",
                    "category": "1",
                    "main_cat": "1",
                    "leechers": 0,
                    "seeders": 0,
                    "vip": 0,
                    "free": 0,
                    "size": input,
                    "added": "",
                    "numfiles": 1,
                    "filetype": ""
                }]
            });
            let resp: MamSearchResponse = serde_json::from_value(json).unwrap();
            let tolerance = (expected as f64 * 0.001) as i64; // 0.1% tolerance for float rounding
            assert!(
                (resp.data[0].size - expected).abs() <= tolerance,
                "input={input}: expected ~{expected}, got {}",
                resp.data[0].size
            );
        }
    }

    #[test]
    fn test_parse_author_info_json() {
        // Single author
        assert_eq!(
            parse_author_info(r#"{"34767":"Educational Testing Service"}"#),
            "Educational Testing Service"
        );

        // Multiple authors
        let result = parse_author_info(r#"{"123":"Author One","456":"Author Two"}"#);
        assert!(result.contains("Author One"));
        assert!(result.contains("Author Two"));
        assert!(result.contains(", "));

        // Empty JSON object
        assert_eq!(parse_author_info("{}"), "");

        // Fallback on non-JSON
        assert_eq!(parse_author_info("John Doe"), "John Doe");

        // HTML fallback
        assert_eq!(parse_author_info("<b>Author</b>"), "Author");
    }

    #[test]
    fn test_mam_user_info_vip_detection() {
        let vip: MamUserInfo = serde_json::from_value(serde_json::json!({
            "classname": "VIP",
            "wedges": 790
        }))
        .unwrap();
        assert!(vip.is_vip());

        let non_vip: MamUserInfo = serde_json::from_value(serde_json::json!({
            "classname": "Power User",
            "wedges": 100
        }))
        .unwrap();
        assert!(!non_vip.is_vip());

        // Missing classname defaults to empty string
        let missing: MamUserInfo = serde_json::from_value(serde_json::json!({})).unwrap();
        assert!(!missing.is_vip());
    }
}
