use axum::http::StatusCode;
use quick_xml::Reader;
use quick_xml::events::Event;
use serde::Deserialize;
use time::OffsetDateTime;
use tracing::debug;
use uuid::Uuid;

use crate::prowlarr::parse_newznab_rss;
use crate::repositories::indexer_rss_cache as cache_repo;
use crate::repositories::watch as watch_repo;
use crate::watch::Release;

use super::types::{
    IndexerCapabilities, IndexerContext, IndexerError, IndexerHandler, IndexerSearchRequest,
    NewznabCaps, RssPageResult, SearchType, SearchTypeCaps,
};

#[derive(Debug, Deserialize)]
struct NewznabConfig {
    base_url: String,
    api_key: Option<String>,
    indexer_id: Option<i32>,
    rss_cache_seconds: Option<i32>,
    /// Opt back in to `t=book` for book/audiobook searches. Off by default:
    /// see `resolve_search_type` in `indexers/types.rs` for why `t=book` is
    /// not trusted on newznab sources.
    #[serde(default)]
    #[allow(dead_code)]
    trust_book_search: Option<bool>,
}

pub struct NewznabIndexer;

#[async_trait::async_trait]
impl IndexerHandler for NewznabIndexer {
    fn handler_type(&self) -> &'static str {
        "newznab"
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
                    "name": "base_url",
                    "label": "Base URL",
                    "type": "text",
                    "required": true,
                    "placeholder": "https://indexer.example.com"
                },
                {
                    "name": "api_key",
                    "label": "API Key",
                    "type": "password",
                    "required": true
                },
                {
                    "name": "rss_cache_seconds",
                    "label": "RSS cache (seconds)",
                    "type": "number"
                },
                {
                    "name": "trust_book_search",
                    "label": "Trust this indexer's book search (t=book)",
                    "type": "checkbox",
                    "default": false,
                    "help": "Off by default. Most newznab indexers ignore the query (and sometimes the category) on t=book and return their whole index. Leave off to search books as a categorised generic search, which returns relevant results."
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

        let config: NewznabConfig = serde_json::from_value(source.handler_config.clone())
            .map_err(|e| IndexerError::new(StatusCode::BAD_REQUEST, e.to_string()))?;

        if config.base_url.trim().is_empty() {
            return Err(IndexerError::new(
                StatusCode::BAD_REQUEST,
                "newznab base_url not configured",
            ));
        }

        let base = config.base_url.trim_end_matches('/');
        let url = if base.ends_with("/api") {
            base.to_string()
        } else {
            format!("{}/api", base)
        };

        let categories = if request.categories.is_empty() {
            source.category_ids.clone().unwrap_or_default()
        } else {
            request.categories.clone()
        };

        let t = match request.search_type {
            SearchType::TvSearch => "tvsearch",
            SearchType::MovieSearch => "movie",
            SearchType::BookSearch => "book",
            SearchType::MusicSearch => "music",
            SearchType::Generic => "search",
        };

        let mut params: Vec<(String, String)> = Vec::new();
        params.push(("t".to_string(), t.to_string()));
        // Only send q param when non-empty — structured searches (ID-based) don't need it
        if !request.query.is_empty() {
            params.push(("q".to_string(), request.query));
        }
        if let Some(ref api_key) = config.api_key {
            params.push(("apikey".to_string(), api_key.clone()));
        }
        if !categories.is_empty() {
            let joined = categories
                .iter()
                .map(|c| c.to_string())
                .collect::<Vec<_>>()
                .join(",");
            params.push(("cat".to_string(), joined));
        }
        if let Some(limit) = request.limit {
            params.push(("limit".to_string(), limit.to_string()));
        }
        if let Some(offset) = request.offset {
            params.push(("offset".to_string(), offset.to_string()));
        }

        // Pass structured season/episode params for TV searches
        if let Some(ref season) = request.season {
            params.push(("season".to_string(), season.clone()));
        }
        if let Some(ref ep) = request.episode {
            params.push(("ep".to_string(), ep.clone()));
        }

        // Pass external identifiers as Newznab query params
        for ext_id in &request.external_ids {
            let param_name = match &ext_id.source {
                crate::watch::ExternalIdSource::Imdb => "imdbid",
                crate::watch::ExternalIdSource::Tmdb => "tmdbid",
                crate::watch::ExternalIdSource::Tvdb => "tvdbid",
                crate::watch::ExternalIdSource::TvMaze => "tvmazeid",
                _ => continue,
            };
            if !ext_id.id.is_empty() {
                params.push((param_name.to_string(), ext_id.id.clone()));
            }
        }

        let resp = ctx
            .http_client
            .get(&url)
            .query(&params)
            .send()
            .await
            .map_err(|e| IndexerError::new(StatusCode::BAD_GATEWAY, e.to_string()))?;

        if !resp.status().is_success() {
            return Err(IndexerError::bad_status("newznab", resp).await);
        }

        let body = resp
            .text()
            .await
            .map_err(|e| IndexerError::new(StatusCode::BAD_GATEWAY, e.to_string()))?;

        let indexer_id = config.indexer_id.unwrap_or(0);
        let releases = parse_newznab_rss(&body, indexer_id)
            .map_err(|e| IndexerError::new(StatusCode::BAD_GATEWAY, e.to_string()))?;

        // Parse and store API limits if the response includes them
        if let Some(limits) = crate::prowlarr::parse_api_limits(&body) {
            let _ = crate::repositories::watch::update_api_limits_from_response(
                ctx.pool,
                source.id,
                limits.api_current,
                limits.api_max,
            )
            .await;
        }

        let mut releases: Vec<Release> = releases.into_iter().map(Release::from).collect();
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
        let result = self.rss_page(ctx, source, 0, 100).await?;
        Ok(result.releases)
    }

    async fn rss_page(
        &self,
        ctx: &IndexerContext<'_>,
        source: &watch_repo::IndexerSourceRow,
        offset: u32,
        limit: u32,
    ) -> Result<RssPageResult, IndexerError> {
        let config: NewznabConfig = serde_json::from_value(source.handler_config.clone())
            .map_err(|e| IndexerError::new(StatusCode::BAD_REQUEST, e.to_string()))?;

        if config.base_url.trim().is_empty() {
            return Err(IndexerError::new(
                StatusCode::BAD_REQUEST,
                "newznab base_url not configured",
            ));
        }

        let base = config.base_url.trim_end_matches('/');
        let url = if base.ends_with("/api") {
            base.to_string()
        } else {
            format!("{}/api", base)
        };

        let mut params: Vec<(String, String)> = Vec::new();
        params.push(("t".to_string(), "search".to_string()));
        params.push(("limit".to_string(), limit.to_string()));
        params.push(("offset".to_string(), offset.to_string()));
        if let Some(ref api_key) = config.api_key {
            params.push(("apikey".to_string(), api_key.clone()));
        }
        if let Some(categories) = source.category_ids.as_ref()
            && !categories.is_empty()
        {
            let joined = categories
                .iter()
                .map(|c| c.to_string())
                .collect::<Vec<_>>()
                .join(",");
            params.push(("cat".to_string(), joined));
        }

        // Only use cache for page 0
        let body = if offset == 0 {
            fetch_with_cache(ctx, source.id, &url, &params, config.rss_cache_seconds).await?
        } else {
            fetch_direct(ctx, &url, &params).await?
        };

        let indexer_id = config.indexer_id.unwrap_or(0);
        let releases = parse_newznab_rss(&body, indexer_id)
            .map_err(|e| IndexerError::new(StatusCode::BAD_GATEWAY, e.to_string()))?;

        // Parse and store API limits if the response includes them
        if let Some(limits) = crate::prowlarr::parse_api_limits(&body) {
            let _ = crate::repositories::watch::update_api_limits_from_response(
                ctx.pool,
                source.id,
                limits.api_current,
                limits.api_max,
            )
            .await;
        }

        let mut releases: Vec<Release> = releases.into_iter().map(Release::from).collect();
        for release in &mut releases {
            release.indexer = source.name.clone();
            release.indexer_source_id = Some(source.id);
        }

        Ok(RssPageResult {
            releases,
            supports_pagination: true,
        })
    }
}

/// Fetch RSS without caching (for pagination pages beyond page 0).
/// Handles 429 responses with Retry-After header parsing.
async fn fetch_direct(
    ctx: &IndexerContext<'_>,
    url: &str,
    params: &[(String, String)],
) -> Result<String, IndexerError> {
    let resp = ctx
        .http_client
        .get(url)
        .query(params)
        .send()
        .await
        .map_err(|e| IndexerError::new(StatusCode::BAD_GATEWAY, e.to_string()))?;

    if resp.status() == StatusCode::TOO_MANY_REQUESTS {
        let retry_after = resp
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());
        return Err(IndexerError::rate_limited(retry_after));
    }

    if !resp.status().is_success() {
        return Err(IndexerError::bad_status("newznab", resp).await);
    }

    resp.text()
        .await
        .map_err(|e| IndexerError::new(StatusCode::BAD_GATEWAY, e.to_string()))
}

async fn fetch_with_cache(
    ctx: &IndexerContext<'_>,
    source_id: Uuid,
    url: &str,
    params: &[(String, String)],
    rss_cache_seconds: Option<i32>,
) -> Result<String, IndexerError> {
    let ttl_seconds = rss_cache_seconds.unwrap_or(900).max(0);
    if ttl_seconds > 0
        && let Ok(Some(cache)) = cache_repo::get_rss_cache(ctx.pool, source_id).await
    {
        let expires_at = cache.fetched_at + time::Duration::seconds(cache.ttl_seconds as i64);
        if OffsetDateTime::now_utc() < expires_at {
            return Ok(cache.response_xml);
        }
    }

    let resp = ctx
        .http_client
        .get(url)
        .query(params)
        .send()
        .await
        .map_err(|e| IndexerError::new(StatusCode::BAD_GATEWAY, e.to_string()))?;

    if resp.status() == StatusCode::TOO_MANY_REQUESTS {
        let retry_after = resp
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());
        return Err(IndexerError::rate_limited(retry_after));
    }

    if !resp.status().is_success() {
        return Err(IndexerError::bad_status("newznab", resp).await);
    }

    let body = resp
        .text()
        .await
        .map_err(|e| IndexerError::new(StatusCode::BAD_GATEWAY, e.to_string()))?;

    if ttl_seconds > 0 {
        let _ = cache_repo::upsert_rss_cache(
            ctx.pool,
            source_id,
            OffsetDateTime::now_utc(),
            ttl_seconds,
            &body,
        )
        .await;
    }

    Ok(body)
}

/// Fetch capabilities from a Newznab indexer's `/api?t=caps` endpoint.
///
/// Requires the source's `handler_config` to contain `base_url` and optionally `api_key`.
pub async fn fetch_caps(
    http_client: &reqwest::Client,
    source: &watch_repo::IndexerSourceRow,
) -> Result<NewznabCaps, IndexerError> {
    let config: NewznabConfig = serde_json::from_value(source.handler_config.clone())
        .map_err(|e| IndexerError::new(StatusCode::BAD_REQUEST, e.to_string()))?;

    if config.base_url.trim().is_empty() {
        return Err(IndexerError::new(
            StatusCode::BAD_REQUEST,
            "newznab base_url not configured",
        ));
    }

    let base = config.base_url.trim_end_matches('/');
    let url = if base.ends_with("/api") {
        base.to_string()
    } else {
        format!("{}/api", base)
    };

    let mut params = vec![("t".to_string(), "caps".to_string())];
    if let Some(ref api_key) = config.api_key {
        params.push(("apikey".to_string(), api_key.clone()));
    }

    let resp = http_client
        .get(&url)
        .query(&params)
        .send()
        .await
        .map_err(|e| IndexerError::new(StatusCode::BAD_GATEWAY, e.to_string()))?;

    if resp.status() == StatusCode::TOO_MANY_REQUESTS {
        let retry_after = resp
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok());
        return Err(IndexerError::rate_limited(retry_after));
    }

    if !resp.status().is_success() {
        return Err(IndexerError::bad_status("newznab caps", resp).await);
    }

    let body = resp
        .text()
        .await
        .map_err(|e| IndexerError::new(StatusCode::BAD_GATEWAY, e.to_string()))?;

    debug!(source = %source.name, body_len = body.len(), "fetched caps XML");

    parse_caps_xml(&body)
        .map_err(|e| IndexerError::new(StatusCode::BAD_GATEWAY, format!("caps parse error: {e}")))
}

/// Parse a Newznab caps XML response into structured capabilities.
///
/// Extracts the `<searching>` section with per-search-type availability and supported params.
/// Example input:
/// ```xml
/// <caps>
///   <searching>
///     <search available="yes" supportedParams="q"/>
///     <tv-search available="yes" supportedParams="q,rid,tvdbid,tvmazeid,season,ep"/>
///     <movie-search available="yes" supportedParams="q,imdbid"/>
///     <book-search available="no" supportedParams=""/>
///     <audio-search available="no" supportedParams=""/>
///   </searching>
/// </caps>
/// ```
pub fn parse_caps_xml(xml: &str) -> Result<NewznabCaps, String> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);

    let mut caps = NewznabCaps::default();
    let mut in_searching = false;

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) => {
                let name = e.name().as_ref().to_string();
                if name == "searching" {
                    in_searching = true;
                }
            }
            Ok(Event::End(e)) => {
                let name = e.name().as_ref().to_string();
                if name == "searching" {
                    in_searching = false;
                }
            }
            Ok(Event::Empty(e)) if in_searching => {
                let name = e.name().as_ref().to_string();
                let target = match name.as_str() {
                    "search" => Some(&mut caps.search),
                    "tv-search" => Some(&mut caps.tv_search),
                    "movie-search" => Some(&mut caps.movie_search),
                    "book-search" => Some(&mut caps.book_search),
                    "audio-search" => Some(&mut caps.audio_search),
                    _ => None,
                };
                if let Some(target) = target {
                    *target = parse_search_type_attrs(&e);
                }
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(format!("XML parse error: {e}")),
            _ => {}
        }
    }

    Ok(caps)
}

fn parse_search_type_attrs(e: &quick_xml::events::BytesStart<'_>) -> SearchTypeCaps {
    let mut available = false;
    let mut params = Vec::new();

    for attr in e.attributes().flatten() {
        let key = attr.key.as_ref().to_string();
        let val = attr.value.to_string();
        match key.as_str() {
            "available" => available = val == "yes",
            "supportedParams" => {
                params = val
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
            _ => {}
        }
    }

    SearchTypeCaps { available, params }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_caps_xml_basic() {
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<caps>
  <server version="1.0" title="TestIndexer" />
  <searching>
    <search available="yes" supportedParams="q" />
    <tv-search available="yes" supportedParams="q,rid,tvdbid,tvmazeid,season,ep" />
    <movie-search available="yes" supportedParams="q,imdbid,genre" />
    <book-search available="no" supportedParams="" />
    <audio-search available="no" supportedParams="" />
  </searching>
</caps>"#;

        let caps = parse_caps_xml(xml).unwrap();

        assert!(caps.search.available);
        assert_eq!(caps.search.params, vec!["q"]);

        assert!(caps.tv_search.available);
        assert_eq!(
            caps.tv_search.params,
            vec!["q", "rid", "tvdbid", "tvmazeid", "season", "ep"]
        );
        assert!(caps.supports_param(SearchType::TvSearch, "tvdbid"));
        assert!(caps.supports_param(SearchType::TvSearch, "season"));
        assert!(!caps.supports_param(SearchType::TvSearch, "imdbid"));

        assert!(caps.movie_search.available);
        assert_eq!(caps.movie_search.params, vec!["q", "imdbid", "genre"]);
        assert!(caps.supports_param(SearchType::MovieSearch, "imdbid"));

        assert!(!caps.book_search.available);
        assert!(caps.book_search.params.is_empty());

        assert!(!caps.audio_search.available);
    }

    #[test]
    fn test_parse_caps_xml_nzbfinder_style() {
        // NZBFinder has more params including tmdbid, traktid
        let xml = r#"<?xml version="1.0" encoding="UTF-8"?>
<caps>
  <searching>
    <search available="yes" supportedParams="q" />
    <tv-search available="yes" supportedParams="q,vid,tvdbid,traktid,rid,tvmazeid,imdbid,tmdbid,season,ep" />
    <movie-search available="yes" supportedParams="q,imdbid,tmdbid,traktid" />
    <book-search available="no" supportedParams="" />
    <audio-search available="no" supportedParams="" />
  </searching>
</caps>"#;

        let caps = parse_caps_xml(xml).unwrap();

        assert!(caps.supports_param(SearchType::TvSearch, "tmdbid"));
        assert!(caps.supports_param(SearchType::TvSearch, "imdbid"));
        assert!(caps.supports_param(SearchType::MovieSearch, "tmdbid"));
        assert!(caps.supports_param(SearchType::MovieSearch, "traktid"));
    }

    #[test]
    fn test_parse_caps_xml_empty_searching() {
        let xml = r#"<caps><searching></searching></caps>"#;
        let caps = parse_caps_xml(xml).unwrap();
        assert!(!caps.search.available);
        assert!(!caps.tv_search.available);
    }

    #[test]
    fn test_parse_caps_xml_no_searching_section() {
        let xml = r#"<caps><server version="1.0" /></caps>"#;
        let caps = parse_caps_xml(xml).unwrap();
        // All defaults — nothing available
        assert!(!caps.search.available);
        assert!(!caps.tv_search.available);
        assert!(!caps.movie_search.available);
    }

    #[test]
    fn test_caps_serialization_roundtrip() {
        let caps = NewznabCaps {
            search: SearchTypeCaps {
                available: true,
                params: vec!["q".to_string()],
            },
            tv_search: SearchTypeCaps {
                available: true,
                params: vec![
                    "q".to_string(),
                    "tvdbid".to_string(),
                    "season".to_string(),
                    "ep".to_string(),
                ],
            },
            ..Default::default()
        };

        let json = serde_json::to_value(&caps).unwrap();
        let restored: NewznabCaps = serde_json::from_value(json).unwrap();
        assert_eq!(restored.tv_search.params.len(), 4);
        assert!(restored.search.available);
        assert!(!restored.movie_search.available);
    }
}
