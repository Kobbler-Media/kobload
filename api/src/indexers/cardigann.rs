//! Cardigann YAML definition indexer handler.
//!
//! Thin adapter between the `cardigann` crate and kobload's `IndexerHandler` trait.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;

use axum::http::StatusCode;
use dashmap::DashMap;
use tracing::{info, warn};

use crate::repositories::watch as watch_repo;
use crate::watch::{ExternalId, ExternalIdSource, Release};

use super::types::{
    IndexerCapabilities, IndexerContext, IndexerError, IndexerHandler, IndexerSearchRequest,
    SearchType,
};

/// In-memory cache of loaded YAML definitions.
pub type DefinitionCache = Arc<DashMap<String, cardigann::Definition>>;

pub struct CardigannIndexer {
    definitions: DefinitionCache,
}

impl CardigannIndexer {
    pub fn new(definitions: DefinitionCache) -> Self {
        Self { definitions }
    }
}

#[async_trait::async_trait]
impl IndexerHandler for CardigannIndexer {
    fn handler_type(&self) -> &'static str {
        "cardigann"
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
        source: Option<&watch_repo::IndexerSourceRow>,
        request: IndexerSearchRequest,
    ) -> Result<Vec<Release>, IndexerError> {
        let source =
            source.ok_or_else(|| IndexerError::new(StatusCode::BAD_REQUEST, "source required"))?;

        let (def, config) = self.load_definition_for_source(source)?;

        let engine = cardigann::CardigannEngine::new(ctx.http_client.clone());

        let mut external_ids = HashMap::new();
        for ext_id in &request.external_ids {
            let key = match &ext_id.source {
                ExternalIdSource::Imdb => "imdbid",
                ExternalIdSource::Tmdb => "tmdbid",
                ExternalIdSource::Tvdb => "tvdbid",
                ExternalIdSource::TvMaze => "tvmazeid",
                _ => continue,
            };
            external_ids.insert(key.to_string(), ext_id.id.clone());
        }

        let search_type = match request.search_type {
            SearchType::TvSearch => cardigann::SearchType::TvSearch,
            SearchType::MovieSearch => cardigann::SearchType::MovieSearch,
            SearchType::BookSearch => cardigann::SearchType::BookSearch,
            SearchType::MusicSearch => cardigann::SearchType::MusicSearch,
            SearchType::Generic => cardigann::SearchType::Search,
        };

        let search_req = cardigann::SearchRequest {
            query: request.query,
            categories: request.categories,
            search_type,
            external_ids,
            limit: request.limit.map(|l| l as i32),
            offset: None,
            season: request.season,
            episode: request.episode,
        };

        let results = engine
            .search(&def, &config, search_req)
            .await
            .map_err(|e| IndexerError::new(StatusCode::BAD_GATEWAY, e.to_string()))?;

        Ok(results
            .into_iter()
            .map(|r| convert_result(r, source))
            .collect())
    }

    async fn rss(
        &self,
        ctx: &IndexerContext<'_>,
        source: &watch_repo::IndexerSourceRow,
    ) -> Result<Vec<Release>, IndexerError> {
        let (def, config) = self.load_definition_for_source(source)?;
        let engine = cardigann::CardigannEngine::new(ctx.http_client.clone());

        let results = engine
            .rss(&def, &config)
            .await
            .map_err(|e| IndexerError::new(StatusCode::BAD_GATEWAY, e.to_string()))?;

        Ok(results
            .into_iter()
            .map(|r| convert_result(r, source))
            .collect())
    }
}

impl CardigannIndexer {
    fn load_definition_for_source(
        &self,
        source: &watch_repo::IndexerSourceRow,
    ) -> Result<(cardigann::Definition, HashMap<String, String>), IndexerError> {
        let definition_name = source
            .handler_config
            .get("definition")
            .and_then(|v| v.as_str())
            .ok_or_else(|| {
                IndexerError::new(
                    StatusCode::BAD_REQUEST,
                    "missing 'definition' in handler_config",
                )
            })?;

        let def = self
            .definitions
            .get(definition_name)
            .map(|r| r.value().clone())
            .ok_or_else(|| {
                IndexerError::new(
                    StatusCode::BAD_REQUEST,
                    format!("definition '{}' not found", definition_name),
                )
            })?;

        // Extract config from handler_config (everything except "definition")
        let config: HashMap<String, String> = source
            .handler_config
            .as_object()
            .map(|obj| {
                obj.iter()
                    .filter(|(k, _)| *k != "definition")
                    .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
                    .collect()
            })
            .unwrap_or_default();

        Ok((def, config))
    }
}

/// Load all YAML definitions from a directory into a `DefinitionCache`.
///
/// Scans for `.yml` and `.yaml` files. If an override directory is provided,
/// files there take precedence over the base directory by filename.
pub fn load_definitions(base_dir: &Path, override_dir: Option<&Path>) -> DefinitionCache {
    let cache = DefinitionCache::default();

    let mut loaded = 0;
    for dir in std::iter::once(Some(base_dir)).chain(std::iter::once(override_dir)) {
        let Some(dir) = dir else { continue };
        let Ok(entries) = std::fs::read_dir(dir) else {
            warn!(dir = %dir.display(), "could not read definitions directory");
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
            if ext != "yml" && ext != "yaml" {
                continue;
            }
            let name = path
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .to_string();
            if name.is_empty() {
                continue;
            }
            match std::fs::read_to_string(&path) {
                Ok(yaml) => match cardigann::load_definition(&yaml) {
                    Ok(def) => {
                        cache.insert(name, def);
                        loaded += 1;
                    }
                    Err(e) => {
                        warn!(file = %path.display(), error = %e, "failed to parse definition")
                    }
                },
                Err(e) => warn!(file = %path.display(), error = %e, "failed to read definition"),
            }
        }
    }
    info!(count = loaded, "loaded cardigann definitions");
    cache
}

/// Reload definitions into an existing cache (clears and repopulates).
pub fn reload_definitions(cache: &DefinitionCache, base_dir: &Path, override_dir: Option<&Path>) {
    cache.clear();
    let fresh = load_definitions(base_dir, override_dir);
    for entry in fresh.iter() {
        cache.insert(entry.key().clone(), entry.value().clone());
    }
}

fn convert_result(r: cardigann::CardigannResult, source: &watch_repo::IndexerSourceRow) -> Release {
    let protocol = if r.magnet_url.is_some() || r.info_hash.is_some() {
        "torrent"
    } else {
        "usenet"
    };

    let external_ids: Vec<ExternalId> = r
        .external_ids
        .iter()
        .filter_map(|(k, v)| {
            let source = match k.as_str() {
                "imdbid" => ExternalIdSource::Imdb,
                "tmdbid" => ExternalIdSource::Tmdb,
                "tvdbid" => ExternalIdSource::Tvdb,
                "tvmazeid" => ExternalIdSource::TvMaze,
                _ => return None,
            };
            Some(ExternalId {
                source,
                id: v.clone(),
            })
        })
        .collect();

    let raw_data = serde_json::json!({
        "protocol": protocol,
        "download_url": &r.download_url,
        "magnet_url": &r.magnet_url,
        "category_ids": &r.category_ids,
    });

    Release {
        guid: r.guid,
        title: r.title,
        file_name: None,
        download_url: r.download_url,
        magnet_url: r.magnet_url,
        info_url: r.info_url,
        size: r.size,
        publish_date: r.publish_date,
        category_ids: r.category_ids,
        indexer: source.name.clone(),
        protocol: protocol.to_string(),
        info_hash: r.info_hash,
        files: r.files,
        seeders: r.seeders,
        peers: None,
        leechers: r.leechers,
        grabs: r.grabs,
        votes: None,
        indexer_id: None,
        indexer_source_id: Some(source.id),
        raw_data,
        external_ids,
        media: None,
    }
}
