//! Handler for search_request jobs.

use std::sync::Arc;

use crate::indexers::IndexerResolver;
use crate::jobs::JobError;
use crate::jobs::worker::JobHandler;
use crate::services::search::{SearchContext, SearchServiceContext, SearchServiceRequest, search};
use async_trait::async_trait;
use nats_jobs::{SearchRequest, SearchResponse, SearchResult as NatsSearchResult};
use serde_json::Value;
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;
use tracing::info;
use uuid::Uuid;

pub struct SearchRequestHandler {
    pool: PgPool,
    http_client: reqwest::Client,
    indexers: Arc<dyn IndexerResolver>,
}

impl SearchRequestHandler {
    pub fn new(
        pool: PgPool,
        http_client: reqwest::Client,
        indexers: Arc<dyn IndexerResolver>,
    ) -> Self {
        Self {
            pool,
            http_client,
            indexers,
        }
    }
}

#[async_trait]
impl JobHandler for SearchRequestHandler {
    fn job_type(&self) -> &'static str {
        "search_request"
    }

    fn max_concurrent(&self) -> usize {
        5
    }

    async fn execute(
        &self,
        job_id: Uuid,
        payload: Value,
        cancel: CancellationToken,
    ) -> Result<Value, JobError> {
        nats_jobs::check_cancelled(&cancel)?;

        let request: SearchRequest = serde_json::from_value(payload)?;

        info!(
            job_id = %job_id,
            query = %request.query,
            media_type = ?request.media_type,
            "Processing search_request job"
        );

        let external_ids = request
            .media_want
            .as_ref()
            .map(crate::media::external_ids_from_media_want)
            .unwrap_or_default();

        // Resolve media_type from key string
        let media_type_id = if let Some(ref mt) = request.media_type {
            crate::repositories::media_types::get_by_key(&self.pool, mt)
                .await
                .ok()
                .flatten()
                .map(|row| row.id)
        } else {
            None
        };

        let service_request = SearchServiceRequest {
            query: request.query.clone(),
            media_type_id,
            protocol: request.protocol.clone(),
            limit: request.limit,
            offset: request.offset,
            media_want: request.media_want.clone(),
            external_ids,
            search_context: SearchContext::Interactive,
        };

        let ctx = SearchServiceContext {
            pool: &self.pool,
            http_client: &self.http_client,
            indexers: self.indexers.as_ref(),
        };

        let result = search(&ctx, service_request, None, None)
            .await
            .map_err(|e| JobError::ExecutionFailed(e.message))?;

        let response = SearchResponse {
            results: result.results.into_iter().map(map_search_result).collect(),
            errors: result.errors,
        };

        serde_json::to_value(response).map_err(JobError::from)
    }
}

fn map_search_result(result: crate::types::SearchResult) -> NatsSearchResult {
    let (seeders, leechers) = match result.torrent.as_ref() {
        Some(torrent) => (torrent.seeders, torrent.leechers),
        None => (None, None),
    };
    let grabs = result.usenet.as_ref().and_then(|usenet| usenet.grabs);

    NatsSearchResult {
        guid: result.guid.unwrap_or(result.id),
        title: result.title,
        indexer: result.indexer,
        size: Some(result.size),
        publish_date: result.publish_date,
        download_url: result.download_url,
        magnet_url: result.magnet_url,
        protocol: Some(result.protocol),
        info_url: result.info_url,
        info_hash: result.info_hash,
        indexer_id: result.indexer_id,
        indexer_type: Some(result.indexer_type),
        seeders,
        leechers,
        grabs,
    }
}
