use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;

use crate::repositories::watch as watch_repo;
use crate::watch::{ExternalId, Release};

/// Per-search-type capability info from a Newznab caps endpoint.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SearchTypeCaps {
    pub available: bool,
    pub params: Vec<String>,
}

/// Parsed capabilities from a Newznab `/api?t=caps` response.
/// Stored as JSONB in `indexer_sources.capabilities`.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct NewznabCaps {
    pub search: SearchTypeCaps,
    pub tv_search: SearchTypeCaps,
    pub movie_search: SearchTypeCaps,
    pub book_search: SearchTypeCaps,
    pub audio_search: SearchTypeCaps,
}

impl NewznabCaps {
    /// Check if the given search type supports a specific parameter (e.g. "imdbid", "season").
    pub fn supports_param(&self, search_type: SearchType, param: &str) -> bool {
        let caps = match search_type {
            SearchType::Generic => &self.search,
            SearchType::TvSearch => &self.tv_search,
            SearchType::MovieSearch => &self.movie_search,
            SearchType::BookSearch => &self.book_search,
            SearchType::MusicSearch => &self.audio_search,
        };
        caps.available && caps.params.iter().any(|p| p == param)
    }

    /// Check if the given search type is available at all.
    pub fn search_available(&self, search_type: SearchType) -> bool {
        match search_type {
            SearchType::Generic => self.search.available,
            SearchType::TvSearch => self.tv_search.available,
            SearchType::MovieSearch => self.movie_search.available,
            SearchType::BookSearch => self.book_search.available,
            SearchType::MusicSearch => self.audio_search.available,
        }
    }
}

/// Resolve the search type to actually send to one source.
///
/// `t=book` is unusable on every Usenet indexer we have: some ignore both `q`
/// and `cat` and return the whole index (altHUB answered a book query with
/// German TV rips, Usenet Crawler with 32M techno releases), and the one that
/// advertises `book_search` returns the same newest-ebooks page whatever you
/// ask it. `t=search` with `cat=7020,7000` returns clean, query-relevant book
/// results on those same sources, so books are downgraded to a categorised
/// generic search unless a source explicitly opts back in via
/// `handler_config.trust_book_search`.
///
/// Only newznab/torznab are affected — cardigann, MAM and TorrentDay map book
/// searches through their own engines and handle them correctly.
pub fn resolve_search_type(
    source: &watch_repo::IndexerSourceRow,
    search_type: SearchType,
) -> SearchType {
    if search_type != SearchType::BookSearch {
        return search_type;
    }
    if source.handler_type != "newznab" && source.handler_type != "torznab" {
        return search_type;
    }
    let trusted = source
        .handler_config
        .get("trust_book_search")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);
    if trusted {
        search_type
    } else {
        SearchType::Generic
    }
}

/// Result from a paginated RSS fetch.
pub struct RssPageResult {
    pub releases: Vec<Release>,
    /// Whether the indexer actually supports offset-based pagination.
    /// If false, the caller should not attempt further pages.
    pub supports_pagination: bool,
}

/// The type of search to perform against an indexer.
///
/// Maps to newznab `t` parameter values and cardigann search modes.
/// Using the correct search type enables ID-based search (e.g. `tvdbid`
/// with `t=tvsearch`) which dramatically improves result quality.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SearchType {
    #[default]
    Generic,
    TvSearch,
    MovieSearch,
    BookSearch,
    MusicSearch,
}

#[derive(Debug, Clone, Copy)]
pub struct IndexerCapabilities {
    pub supports_search: bool,
    pub supports_rss: bool,
}

#[derive(Debug, Clone)]
pub struct IndexerContext<'a> {
    pub pool: &'a PgPool,
    pub http_client: &'a reqwest::Client,
}

#[derive(Debug, Clone)]
pub struct IndexerSearchRequest {
    pub query: String,
    pub categories: Vec<i32>,
    pub limit: Option<u32>,
    /// Offset for pagination (skip first N results).
    pub offset: Option<u32>,
    /// External identifiers for ID-based search (IMDB, TMDB, etc.).
    pub external_ids: Vec<ExternalId>,
    /// The type of search to perform (generic, tv, movie, book, music).
    pub search_type: SearchType,
    /// Season identifier for structured TV searches (Newznab `season` param).
    pub season: Option<String>,
    /// Episode identifier for structured TV searches (Newznab `ep` param).
    pub episode: Option<String>,
}

#[derive(Debug)]
pub struct IndexerError {
    pub status: StatusCode,
    pub message: String,
    /// For 429 responses: parsed Retry-After header value in seconds.
    pub retry_after_seconds: Option<u64>,
    /// The indexer rejected our credentials. Retrying cannot fix this — the
    /// stored config needs a human — so it is recorded separately from the
    /// transient failures that drive exponential backoff.
    pub auth_failed: bool,
}

impl IndexerError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            retry_after_seconds: None,
            auth_failed: false,
        }
    }

    pub fn rate_limited(retry_after_seconds: Option<u64>) -> Self {
        Self {
            status: StatusCode::TOO_MANY_REQUESTS,
            message: format!("rate limited, retry after {:?}s", retry_after_seconds),
            retry_after_seconds,
            auth_failed: false,
        }
    }

    /// The indexer refused our stored credentials (expired cookie/session, revoked key).
    pub fn auth_failed(message: impl Into<String>) -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            message: message.into(),
            retry_after_seconds: None,
            auth_failed: true,
        }
    }

    /// Build a diagnostic error for non-success HTTP responses.
    ///
    /// Includes status code and a truncated body snippet so the stored
    /// `last_error` tells you *what* the server actually returned
    /// (e.g. an HTML login page when a cookie expires).
    ///
    /// 401/403 are reported as credential rejections: for the cookie and
    /// API-key handlers that is a dead session or a revoked key, and retrying
    /// on a backoff will never recover it.
    pub async fn bad_status(name: &str, resp: reqwest::Response) -> Self {
        let status = resp.status();
        let content_type = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("unknown")
            .to_string();
        let body = resp.text().await.unwrap_or_default();
        let snippet = body_snippet(&body);
        let message = format!("{name} returned {status} ({content_type}): {snippet}");
        if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
            Self::auth_failed(message)
        } else {
            Self::new(StatusCode::BAD_GATEWAY, message)
        }
    }

    /// Build a diagnostic error when JSON deserialization fails.
    ///
    /// Shows the content-type and a body snippet so you can tell whether
    /// the server returned HTML (expired cookie), XML, or malformed JSON.
    pub fn json_parse_failed(
        name: &str,
        content_type: &str,
        body: &str,
        err: impl std::fmt::Display,
    ) -> Self {
        let snippet = body_snippet(body);
        Self::new(
            StatusCode::BAD_GATEWAY,
            format!("{name} JSON parse failed ({content_type}): {err} — body: {snippet}"),
        )
    }
}

/// Truncate a response body to a short snippet for error messages.
fn body_snippet(body: &str) -> String {
    let trimmed = body.trim();
    if trimmed.len() <= 200 {
        trimmed.to_string()
    } else {
        format!("{}…", &trimmed[..200])
    }
}

impl std::fmt::Display for IndexerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for IndexerError {}

#[async_trait::async_trait]
pub trait IndexerHandler: Send + Sync {
    fn handler_type(&self) -> &'static str;
    fn capabilities(&self) -> IndexerCapabilities;

    async fn search(
        &self,
        ctx: &IndexerContext<'_>,
        source: Option<&watch_repo::IndexerSourceRow>,
        request: IndexerSearchRequest,
    ) -> Result<Vec<Release>, IndexerError>;

    async fn rss(
        &self,
        ctx: &IndexerContext<'_>,
        source: &watch_repo::IndexerSourceRow,
    ) -> Result<Vec<Release>, IndexerError>;

    /// Fetch a single page of RSS results with offset/limit for pagination.
    /// Default implementation calls `rss()` and ignores offset/limit.
    /// Override for indexers that support Newznab-style `offset` parameter.
    async fn rss_page(
        &self,
        ctx: &IndexerContext<'_>,
        source: &watch_repo::IndexerSourceRow,
        _offset: u32,
        _limit: u32,
    ) -> Result<RssPageResult, IndexerError> {
        let releases = self.rss(ctx, source).await?;
        Ok(RssPageResult {
            releases,
            supports_pagination: false,
        })
    }

    /// Pre-download hook (e.g., buy freeleech wedge on MAM).
    /// Returns the (possibly modified) download URL.
    async fn prepare_download(
        &self,
        _ctx: &IndexerContext<'_>,
        _source: &watch_repo::IndexerSourceRow,
        download_url: &str,
    ) -> Result<String, IndexerError> {
        Ok(download_url.to_string())
    }

    /// JSON Schema describing handler_config fields for dynamic UI rendering.
    fn config_schema(&self) -> serde_json::Value {
        serde_json::json!({})
    }

    /// Test connection with the given handler_config. Returns Ok(message) on success.
    async fn test_connection(
        &self,
        _ctx: &IndexerContext<'_>,
        _config: &serde_json::Value,
    ) -> Result<String, IndexerError> {
        Err(IndexerError::new(
            StatusCode::NOT_IMPLEMENTED,
            "test connection not supported for this handler",
        ))
    }
}
