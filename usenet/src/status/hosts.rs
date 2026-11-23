use std::collections::HashMap;
use std::sync::{Arc, PoisonError, RwLock};

use usenet_proto::UsenetServer;

const UNKNOWN: &str = "unknown host";

// A provider's display name is free text an admin may have put an account name in, so the
// status only ever names the host from the config.
#[derive(Clone, Default)]
pub struct ProviderHosts(Arc<RwLock<HashMap<String, String>>>);

impl ProviderHosts {
    pub fn replace(&self, servers: &[UsenetServer]) {
        let hosts = servers
            .iter()
            .map(|s| (s.id.clone(), s.host.clone()))
            .collect();
        *self.0.write().unwrap_or_else(PoisonError::into_inner) = hosts;
    }

    #[must_use]
    pub fn host(&self, key: &str) -> String {
        self.0
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .get(key)
            .map_or_else(|| UNKNOWN.to_owned(), Clone::clone)
    }
}
