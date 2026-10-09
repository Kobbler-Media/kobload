use std::sync::Arc;

use super::cardigann::{CardigannIndexer, DefinitionCache};
use super::mam::MamIndexer;
use super::newznab::NewznabIndexer;
use super::prowlarr::ProwlarrIndexer;
use super::torrentday::TorrentDayIndexer;
use super::types::IndexerHandler;

/// Trait for resolving indexer handlers by type.
/// Inject via AppState to allow test mocks.
pub trait IndexerResolver: Send + Sync {
    fn get(&self, handler_type: &str) -> Option<Arc<dyn IndexerHandler>>;
}

/// Default resolver that returns the real indexer implementations.
pub struct DefaultIndexerResolver {
    definitions: DefinitionCache,
}

impl DefaultIndexerResolver {
    pub fn new(definitions: DefinitionCache) -> Self {
        Self { definitions }
    }
}

impl IndexerResolver for DefaultIndexerResolver {
    fn get(&self, handler_type: &str) -> Option<Arc<dyn IndexerHandler>> {
        match handler_type {
            "prowlarr" => Some(Arc::new(ProwlarrIndexer)),
            "newznab" | "torznab" => Some(Arc::new(NewznabIndexer)),
            "cardigann" => Some(Arc::new(CardigannIndexer::new(self.definitions.clone()))),
            "mam" => Some(Arc::new(MamIndexer)),
            "torrentday" => Some(Arc::new(TorrentDayIndexer)),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_resolver_lookup() {
        let definitions = DefinitionCache::default();
        let resolver = DefaultIndexerResolver::new(definitions);
        assert!(resolver.get("prowlarr").is_some());
        assert!(resolver.get("newznab").is_some());
        assert!(resolver.get("torznab").is_some());
        assert!(resolver.get("cardigann").is_some());
        assert!(resolver.get("mam").is_some());
        assert!(resolver.get("torrentday").is_some());
        assert!(resolver.get("unknown").is_none());
    }
}
