//! Core engine: executes search/RSS queries against a loaded definition.

use std::collections::HashMap;

use scraper::{Html, Selector};
use tracing::warn;

use crate::definition::{Definition, FieldDef, SearchPathDef, StringOrVec};
use crate::filters::apply_filters;
use crate::login;
use crate::selector::extract_field_html;
use crate::template;

/// Search type for ID-based searches.
#[derive(Debug, Clone)]
pub enum SearchType {
    Search,
    TvSearch,
    MovieSearch,
    BookSearch,
    MusicSearch,
}

/// Search request passed to the engine.
#[derive(Debug, Clone)]
pub struct SearchRequest {
    pub query: String,
    pub categories: Vec<i32>,
    pub search_type: SearchType,
    pub external_ids: HashMap<String, String>,
    pub limit: Option<i32>,
    pub offset: Option<i32>,
    /// Season identifier for TV searches (populates `{{.Query.Season}}`).
    pub season: Option<String>,
    /// Episode identifier for TV searches (populates `{{.Query.Ep}}`).
    pub episode: Option<String>,
}

/// A single result from a Cardigann search/RSS query.
#[derive(Debug, Clone)]
pub struct CardigannResult {
    pub title: String,
    pub download_url: Option<String>,
    pub magnet_url: Option<String>,
    pub size: i64,
    pub seeders: Option<i32>,
    pub leechers: Option<i32>,
    pub publish_date: String,
    pub info_hash: Option<String>,
    pub category_ids: Vec<i32>,
    pub info_url: Option<String>,
    pub guid: String,
    pub files: Option<i32>,
    pub grabs: Option<i32>,
    pub external_ids: HashMap<String, String>,
    pub extra: HashMap<String, String>,
}

/// The Cardigann engine — holds an HTTP client for making requests.
pub struct CardigannEngine {
    client: reqwest::Client,
}

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error("login error: {0}")]
    Login(#[from] login::LoginError),
    #[error("network error: {0}")]
    Network(String),
    #[error("parse error: {0}")]
    Parse(String),
    #[error("definition error: {0}")]
    Definition(String),
}

impl CardigannEngine {
    pub fn new(client: reqwest::Client) -> Self {
        Self { client }
    }

    /// Load and validate a YAML definition.
    pub fn load_definition(yaml: &str) -> Result<Definition, EngineError> {
        serde_yaml::from_str(yaml).map_err(|e| EngineError::Parse(e.to_string()))
    }

    /// Execute a search query against a definition.
    pub async fn search(
        &self,
        def: &Definition,
        config: &HashMap<String, String>,
        req: SearchRequest,
    ) -> Result<Vec<CardigannResult>, EngineError> {
        // Login if needed
        login::perform_login(&self.client, def, config).await?;

        let base_url = get_base_url(def, config);
        let vars = build_vars(def, config, &req);

        // Determine search paths
        let paths = if def.search.paths.is_empty() {
            vec![SearchPathDef {
                path: def.search.path.clone().unwrap_or_default(),
                categories: Vec::new(),
                method: def.search.method.clone(),
                inputs: HashMap::new(),
                followredirect: None,
                categorydesc: None,
                inheritinputs: None,
            }]
        } else {
            def.search.paths.clone()
        };

        // Fetch all response bodies (async part)
        let mut bodies = Vec::new();
        for path_def in &paths {
            if !path_def.categories.is_empty() && !req.categories.is_empty() {
                let has_match = req.categories.iter().any(|c| {
                    path_def
                        .categories
                        .iter()
                        .any(|pc| pc.as_str() == c.to_string())
                });
                if !has_match {
                    continue;
                }
            }

            let path = template::render(&path_def.path, &vars);
            let url = format!("{}/{}", base_url, path.trim_start_matches('/'));

            let mut inputs: HashMap<String, String> = def
                .search
                .inputs
                .iter()
                .map(|(k, v)| (k.clone(), v.0.clone()))
                .collect();
            for (k, v) in &path_def.inputs {
                inputs.insert(k.clone(), v.0.clone());
            }
            let rendered_inputs: HashMap<String, String> = inputs
                .iter()
                .map(|(k, v)| (k.clone(), template::render(v, &vars)))
                .collect();

            let method = path_def
                .method
                .as_deref()
                .or(def.search.method.as_deref())
                .unwrap_or("get");

            let body = execute_request(
                &self.client,
                &url,
                method,
                &rendered_inputs,
                &def.search.headers,
            )
            .await?;
            bodies.push(body);
        }

        // Parse all responses (sync — avoids scraper types crossing await boundaries)
        let def = def.clone();
        let limit = req.limit;
        tokio::task::spawn_blocking(move || {
            let mut all_results = Vec::new();
            for body in &bodies {
                let results = parse_response(&def, &base_url, body)?;
                all_results.extend(results);
                if let Some(limit) = limit {
                    if all_results.len() >= limit as usize {
                        all_results.truncate(limit as usize);
                        break;
                    }
                }
            }
            Ok(all_results)
        })
        .await
        .map_err(|e| EngineError::Parse(format!("spawn_blocking: {e}")))?
    }

    /// Fetch RSS results (search with empty query).
    pub async fn rss(
        &self,
        def: &Definition,
        config: &HashMap<String, String>,
    ) -> Result<Vec<CardigannResult>, EngineError> {
        self.search(
            def,
            config,
            SearchRequest {
                query: String::new(),
                categories: Vec::new(),
                search_type: SearchType::Search,
                external_ids: HashMap::new(),
                limit: None,
                offset: None,
                season: None,
                episode: None,
            },
        )
        .await
    }

    /// Test login for a definition.
    pub async fn test_login(
        &self,
        def: &Definition,
        config: &HashMap<String, String>,
    ) -> Result<(), EngineError> {
        login::perform_login(&self.client, def, config).await?;
        login::test_login(&self.client, def, config).await?;
        Ok(())
    }
}

fn get_base_url(def: &Definition, config: &HashMap<String, String>) -> String {
    config
        .get("url")
        .or_else(|| config.get("sitelink"))
        .cloned()
        .or_else(|| def.links.first().cloned())
        .unwrap_or_default()
        .trim_end_matches('/')
        .to_string()
}

fn build_vars(
    _def: &Definition,
    config: &HashMap<String, String>,
    req: &SearchRequest,
) -> HashMap<String, String> {
    let mut vars = HashMap::new();

    for (k, v) in config {
        vars.insert(format!("Config.{}", k), v.clone());
    }

    vars.insert("Keywords".to_string(), req.query.clone());
    vars.insert("Query.Keywords".to_string(), req.query.clone());
    vars.insert("Query.Q".to_string(), req.query.clone());

    for (k, v) in &req.external_ids {
        vars.insert(format!("Query.{}", k), v.clone());
    }

    if let Some(ref s) = req.season {
        vars.insert("Query.Season".to_string(), s.clone());
    }
    if let Some(ref e) = req.episode {
        vars.insert("Query.Ep".to_string(), e.clone());
    }

    let cat_str = req
        .categories
        .iter()
        .map(|c| c.to_string())
        .collect::<Vec<_>>()
        .join(",");
    vars.insert("Query.Categories".to_string(), cat_str);

    vars.insert(
        "Today.Year".to_string(),
        chrono::Utc::now().format("%Y").to_string(),
    );

    vars
}

async fn execute_request(
    client: &reqwest::Client,
    url: &str,
    method: &str,
    inputs: &HashMap<String, String>,
    headers: &Option<HashMap<String, StringOrVec>>,
) -> Result<String, EngineError> {
    let mut builder = if method == "post" {
        client.post(url).form(inputs)
    } else {
        client.get(url).query(inputs)
    };

    if let Some(hdrs) = headers {
        for (k, v) in hdrs {
            builder = builder.header(k.as_str(), v.as_str());
        }
    }

    let resp = builder
        .send()
        .await
        .map_err(|e| EngineError::Network(e.to_string()))?;

    if !resp.status().is_success() {
        return Err(EngineError::Network(format!(
            "HTTP {}",
            resp.status().as_u16()
        )));
    }

    resp.text()
        .await
        .map_err(|e| EngineError::Network(e.to_string()))
}

fn parse_response(
    def: &Definition,
    base_url: &str,
    body: &str,
) -> Result<Vec<CardigannResult>, EngineError> {
    let trimmed = body.trim();
    if trimmed.starts_with('[') || trimmed.starts_with('{') {
        parse_json_response(def, body)
    } else {
        parse_html_response(def, base_url, body)
    }
}

fn parse_html_response(
    def: &Definition,
    base_url: &str,
    body: &str,
) -> Result<Vec<CardigannResult>, EngineError> {
    let document = Html::parse_document(body);
    let row_selector = Selector::parse(&def.search.rows.selector)
        .map_err(|_| EngineError::Parse("invalid row selector".to_string()))?;

    let rows: Vec<_> = document.select(&row_selector).collect();
    let skip = def.search.rows.after.unwrap_or(0) as usize;

    let mut results = Vec::new();

    for row in rows.iter().skip(skip) {
        if let Some(result) = extract_result_from_row(row, &def.search.fields, base_url) {
            results.push(result);
        }
    }

    Ok(results)
}

fn extract_result_from_row(
    row: &scraper::ElementRef<'_>,
    fields: &HashMap<String, FieldDef>,
    base_url: &str,
) -> Option<CardigannResult> {
    let title = extract_field_html(row, fields.get("title")?, base_url)?;
    if title.is_empty() {
        return None;
    }

    let download_url = fields
        .get("download")
        .and_then(|f| extract_field_html(row, f, base_url));
    let magnet_url = fields
        .get("magnet")
        .or(fields.get("magneturl"))
        .and_then(|f| extract_field_html(row, f, base_url));
    let info_url = fields
        .get("details")
        .or(fields.get("comments"))
        .and_then(|f| extract_field_html(row, f, base_url));

    let size = fields
        .get("size")
        .and_then(|f| extract_field_html(row, f, base_url))
        .and_then(|s| parse_size(&s))
        .unwrap_or(0);

    let seeders = fields
        .get("seeders")
        .and_then(|f| extract_field_html(row, f, base_url))
        .and_then(|s| parse_int(&s));
    let leechers = fields
        .get("leechers")
        .and_then(|f| extract_field_html(row, f, base_url))
        .and_then(|s| parse_int(&s));
    let grabs = fields
        .get("grabs")
        .and_then(|f| extract_field_html(row, f, base_url))
        .and_then(|s| parse_int(&s));
    let files = fields
        .get("files")
        .and_then(|f| extract_field_html(row, f, base_url))
        .and_then(|s| parse_int(&s));

    let publish_date = fields
        .get("date")
        .and_then(|f| extract_field_html(row, f, base_url))
        .unwrap_or_default();

    let info_hash = fields
        .get("infohash")
        .and_then(|f| extract_field_html(row, f, base_url));

    let category_str = fields
        .get("category")
        .and_then(|f| extract_field_html(row, f, base_url))
        .unwrap_or_default();
    let category_ids: Vec<i32> = category_str
        .split(',')
        .filter_map(|s| s.trim().parse().ok())
        .collect();

    let guid = fields
        .get("guid")
        .and_then(|f| extract_field_html(row, f, base_url))
        .or_else(|| download_url.clone())
        .or_else(|| magnet_url.clone())
        .unwrap_or_else(|| title.clone());

    let mut extra = HashMap::new();
    for key in [
        "downloadvolumefactor",
        "uploadvolumefactor",
        "minimumratio",
        "minimumseedtime",
    ] {
        if let Some(value) = fields
            .get(key)
            .and_then(|f| extract_field_html(row, f, base_url))
        {
            extra.insert(key.to_string(), value);
        }
    }

    let mut external_ids = HashMap::new();
    for key in ["imdbid", "tmdbid", "tvdbid", "tvmazeid", "isbn"] {
        if let Some(value) = fields
            .get(key)
            .and_then(|f| extract_field_html(row, f, base_url))
        {
            if !value.is_empty() {
                external_ids.insert(key.to_string(), value);
            }
        }
    }

    Some(CardigannResult {
        title,
        download_url,
        magnet_url,
        size,
        seeders,
        leechers,
        publish_date,
        info_hash,
        category_ids,
        info_url,
        guid,
        files,
        grabs,
        external_ids,
        extra,
    })
}

fn parse_json_response(def: &Definition, body: &str) -> Result<Vec<CardigannResult>, EngineError> {
    let json: serde_json::Value =
        serde_json::from_str(body).map_err(|e| EngineError::Parse(e.to_string()))?;

    let items = match &json {
        serde_json::Value::Array(arr) => arr.clone(),
        serde_json::Value::Object(_) => {
            let key = def
                .search
                .rows
                .selector
                .trim_start_matches('$')
                .trim_start_matches('.');
            match json.get(key) {
                Some(serde_json::Value::Array(arr)) => arr.clone(),
                _ => {
                    warn!(selector = %def.search.rows.selector, "JSON row selector didn't match array");
                    return Ok(Vec::new());
                }
            }
        }
        _ => return Ok(Vec::new()),
    };

    let mut results = Vec::new();
    for item in &items {
        if let Some(result) = extract_result_from_json(item, &def.search.fields) {
            results.push(result);
        }
    }

    Ok(results)
}

fn extract_result_from_json(
    item: &serde_json::Value,
    fields: &HashMap<String, FieldDef>,
) -> Option<CardigannResult> {
    let title = extract_json_field(item, fields.get("title")?)?;
    if title.is_empty() {
        return None;
    }

    let download_url = fields
        .get("download")
        .and_then(|f| extract_json_field(item, f));
    let magnet_url = fields
        .get("magnet")
        .or(fields.get("magneturl"))
        .and_then(|f| extract_json_field(item, f));
    let info_url = fields
        .get("details")
        .and_then(|f| extract_json_field(item, f));

    let size = fields
        .get("size")
        .and_then(|f| extract_json_field(item, f))
        .and_then(|s| parse_size(&s))
        .unwrap_or(0);

    let seeders = fields
        .get("seeders")
        .and_then(|f| extract_json_field(item, f))
        .and_then(|s| parse_int(&s));
    let leechers = fields
        .get("leechers")
        .and_then(|f| extract_json_field(item, f))
        .and_then(|s| parse_int(&s));
    let grabs = fields
        .get("grabs")
        .and_then(|f| extract_json_field(item, f))
        .and_then(|s| parse_int(&s));
    let files = fields
        .get("files")
        .and_then(|f| extract_json_field(item, f))
        .and_then(|s| parse_int(&s));

    let publish_date = fields
        .get("date")
        .and_then(|f| extract_json_field(item, f))
        .unwrap_or_default();

    let info_hash = fields
        .get("infohash")
        .and_then(|f| extract_json_field(item, f));

    let guid = fields
        .get("guid")
        .and_then(|f| extract_json_field(item, f))
        .or_else(|| download_url.clone())
        .or_else(|| magnet_url.clone())
        .unwrap_or_else(|| title.clone());

    Some(CardigannResult {
        title,
        download_url,
        magnet_url,
        size,
        seeders,
        leechers,
        publish_date,
        info_hash,
        category_ids: Vec::new(),
        info_url,
        guid,
        files,
        grabs,
        external_ids: HashMap::new(),
        extra: HashMap::new(),
    })
}

fn extract_json_field(item: &serde_json::Value, field: &FieldDef) -> Option<String> {
    match field {
        FieldDef::Simple(key) => {
            let val = item.get(key)?;
            Some(json_to_string(val))
        }
        FieldDef::Complex(complex) => {
            if let Some(text) = &complex.text {
                return Some(apply_filters(text.as_str(), &complex.filters));
            }
            let key = complex.selector.as_deref()?;
            let val = item.get(key)?;
            let s = json_to_string(val);
            Some(apply_filters(&s, &complex.filters))
        }
    }
}

fn json_to_string(val: &serde_json::Value) -> String {
    match val {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Number(n) => n.to_string(),
        serde_json::Value::Bool(b) => b.to_string(),
        serde_json::Value::Null => String::new(),
        _ => val.to_string(),
    }
}

/// Parse a size string like "1.5 GB", "750 MB", "1024" (bytes).
fn parse_size(s: &str) -> Option<i64> {
    let s = s.trim().replace(',', "");
    if let Ok(bytes) = s.parse::<i64>() {
        return Some(bytes);
    }

    let re = regex::Regex::new(r"(?i)([\d.]+)\s*(bytes?|kb|mb|gb|tb|kib|mib|gib|tib)").ok()?;
    let caps = re.captures(&s)?;
    let num: f64 = caps[1].parse().ok()?;
    let multiplier = match caps[2].to_lowercase().as_str() {
        "byte" | "bytes" => 1.0,
        "kb" => 1_000.0,
        "mb" => 1_000_000.0,
        "gb" => 1_000_000_000.0,
        "tb" => 1_000_000_000_000.0,
        "kib" => 1_024.0,
        "mib" => 1_048_576.0,
        "gib" => 1_073_741_824.0,
        "tib" => 1_099_511_627_776.0,
        _ => 1.0,
    };
    Some((num * multiplier) as i64)
}

fn parse_int(s: &str) -> Option<i32> {
    s.trim().replace(',', "").parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_size() {
        assert_eq!(parse_size("1.5 GB"), Some(1_500_000_000));
        assert_eq!(parse_size("750 MB"), Some(750_000_000));
        assert_eq!(parse_size("1024"), Some(1024));
        assert_eq!(parse_size("1.2 GiB"), Some(1_288_490_188));
    }

    #[test]
    fn test_parse_int() {
        assert_eq!(parse_int("1,234"), Some(1234));
        assert_eq!(parse_int("42"), Some(42));
        assert_eq!(parse_int("abc"), None);
    }
}
