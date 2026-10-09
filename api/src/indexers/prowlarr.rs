use axum::http::StatusCode;

use serde::Deserialize;

use crate::prowlarr::{ProwlarrClient, ProwlarrError};
use crate::watch::Release;

use super::types::{
    IndexerCapabilities, IndexerContext, IndexerError, IndexerHandler, IndexerSearchRequest,
};

pub struct ProwlarrIndexer;

#[derive(Debug, Deserialize)]
struct ProwlarrConfig {
    #[serde(alias = "prowlarr_base_url")]
    base_url: String,
    #[serde(alias = "prowlarr_api_key")]
    api_key: String,
}

#[derive(Debug, Deserialize)]
struct ProwlarrRssConfig {
    #[serde(alias = "prowlarr_base_url")]
    base_url: String,
    #[serde(alias = "prowlarr_api_key")]
    api_key: String,
    #[serde(alias = "prowlarr_indexer_id")]
    indexer_id: i32,
}

#[async_trait::async_trait]
impl IndexerHandler for ProwlarrIndexer {
    fn handler_type(&self) -> &'static str {
        "prowlarr"
    }

    fn capabilities(&self) -> IndexerCapabilities {
        IndexerCapabilities {
            supports_search: true,
            supports_rss: true,
        }
    }

    async fn search(
        &self,
        ctx: &IndexerContext<'_>,
        source: Option<&crate::repositories::watch::IndexerSourceRow>,
        request: IndexerSearchRequest,
    ) -> Result<Vec<Release>, IndexerError> {
        let source = source.ok_or_else(|| {
            IndexerError::new(StatusCode::BAD_REQUEST, "prowlarr source required")
        })?;

        let config: ProwlarrConfig = serde_json::from_value(source.handler_config.clone())
            .map_err(|e| IndexerError::new(StatusCode::BAD_REQUEST, e.to_string()))?;

        let client = ProwlarrClient::new_with_client(
            config.base_url,
            config.api_key,
            ctx.http_client.clone(),
        );

        let releases = match request.limit {
            Some(limit) => {
                client
                    .search_with_limit(&request.query, &request.categories, limit)
                    .await
            }
            None => client.search(&request.query, &request.categories).await,
        }
        .map_err(map_prowlarr_error)?;

        Ok(releases.into_iter().map(Release::from).collect())
    }

    async fn rss(
        &self,
        ctx: &IndexerContext<'_>,
        source: &crate::repositories::watch::IndexerSourceRow,
    ) -> Result<Vec<Release>, IndexerError> {
        let config: ProwlarrRssConfig = serde_json::from_value(source.handler_config.clone())
            .map_err(|e| IndexerError::new(StatusCode::BAD_REQUEST, e.to_string()))?;

        let client = ProwlarrClient::new_with_client(
            config.base_url,
            config.api_key,
            ctx.http_client.clone(),
        );

        let categories = source.category_ids.clone().unwrap_or_default();
        let releases = client
            .fetch_rss(config.indexer_id, &categories, 100, 0)
            .await
            .map_err(map_prowlarr_error)?;

        let mut releases: Vec<Release> = releases.into_iter().map(Release::from).collect();
        for release in &mut releases {
            release.indexer = source.name.clone();
        }

        Ok(releases)
    }
}

fn map_prowlarr_error(err: ProwlarrError) -> IndexerError {
    match err {
        ProwlarrError::InvalidUrl(e) => {
            IndexerError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        }
        ProwlarrError::Http(e) if e.is_decode() => {
            IndexerError::new(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
        }
        ProwlarrError::Http(e) => IndexerError::new(StatusCode::BAD_GATEWAY, e.to_string()),
        ProwlarrError::Status(status) => IndexerError::new(
            StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::BAD_GATEWAY),
            format!("prowlarr status {}", status),
        ),
        ProwlarrError::RateLimited(retry_after) => IndexerError::new(
            StatusCode::TOO_MANY_REQUESTS,
            format!("rate limited, retry after {:?}s", retry_after),
        ),
    }
}
