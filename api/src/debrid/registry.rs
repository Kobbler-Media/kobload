use std::sync::Arc;

use super::alldebrid::AllDebridProvider;
use super::premiumize::PremiumizeProvider;
use super::provider::DebridProvider;
use super::real_debrid::RealDebridProvider;
use super::torbox::TorBoxProvider;
use super::types::DebridProviderType;

/// Trait for resolving debrid provider handlers by type.
/// Inject via AppState to allow test mocks.
pub trait DebridProviderResolver: Send + Sync {
    fn get(&self, provider_type: &DebridProviderType) -> Arc<dyn DebridProvider>;

    fn get_by_str(&self, provider_type: &str) -> Option<Arc<dyn DebridProvider>> {
        let pt = DebridProviderType::parse(provider_type)?;
        Some(self.get(&pt))
    }
}

/// Default resolver that returns provider handler instances.
pub struct DefaultDebridProviderResolver;

impl DebridProviderResolver for DefaultDebridProviderResolver {
    fn get(&self, provider_type: &DebridProviderType) -> Arc<dyn DebridProvider> {
        match provider_type {
            DebridProviderType::RealDebrid => Arc::new(RealDebridProvider),
            DebridProviderType::AllDebrid => Arc::new(AllDebridProvider),
            DebridProviderType::Premiumize => Arc::new(PremiumizeProvider),
            DebridProviderType::TorBox => Arc::new(TorBoxProvider),
        }
    }
}
