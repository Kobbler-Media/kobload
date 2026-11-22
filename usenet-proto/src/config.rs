use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IpPreference {
    #[default]
    Any,
    V4,
    V6,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsenetServer {
    pub id: String,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub tls: bool,
    pub verify_cert: bool,
    #[serde(default)]
    pub ip_preference: IpPreference,
    pub connections: u32,
    pub priority: u32,
    #[serde(default)]
    pub backbone: Option<String>,
    #[serde(default)]
    pub retention_days: Option<u32>,
    #[serde(default)]
    pub expires_on: Option<String>,
    #[serde(default)]
    pub quota_bytes: Option<u64>,
    pub username: String,
    pub password: String,
}

impl std::fmt::Debug for UsenetServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("UsenetServer")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("host", &self.host)
            .field("port", &self.port)
            .field("connections", &self.connections)
            .field("priority", &self.priority)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineConfigResponse {
    pub servers: Vec<UsenetServer>,
}
