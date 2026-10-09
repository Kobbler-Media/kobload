use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use uuid::Uuid;

use crate::indexers::IndexerResolver;
use crate::repositories::watch::SearchContext;
use crate::types::SearchResult;
use crate::watch::ExternalId;

#[derive(Clone)]
pub struct SearchServiceContext<'a> {
    pub pool: &'a PgPool,
    pub http_client: &'a reqwest::Client,
    pub indexers: &'a dyn IndexerResolver,
}

#[derive(Debug, Clone)]
pub struct SearchServiceRequest {
    pub query: String,
    pub media_type_id: Option<Uuid>,
    pub protocol: Option<String>,
    pub limit: Option<u32>,
    /// Offset for pagination (skip first N results).
    pub offset: Option<u32>,
    pub media_want: Option<nats_jobs::MediaWant>,
    pub external_ids: Vec<ExternalId>,
    /// Search context for throttling decisions. Defaults to Interactive.
    pub search_context: SearchContext,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchServiceResult {
    pub results: Vec<SearchResult>,
    pub errors: Vec<String>,
}

#[derive(Debug)]
pub struct SearchServiceError {
    pub status: StatusCode,
    pub message: String,
}

impl SearchServiceError {
    pub fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for SearchServiceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for SearchServiceError {}
