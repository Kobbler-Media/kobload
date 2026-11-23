use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImportMode {
    Stream,
    Prewarm,
    Download,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct ImportRequest {
    pub cache_key: String,
    pub name: String,
    pub mode: ImportMode,
    #[serde(default)]
    pub password: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nzb_base64: Option<String>,
}

impl std::fmt::Debug for ImportRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ImportRequest")
            .field("cache_key", &self.cache_key)
            .field("name", &self.name)
            .field("mode", &self.mode)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutputInfo {
    pub index: usize,
    pub path: String,
    pub size: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportResponse {
    pub release_key: String,
    pub outputs: Vec<OutputInfo>,
}
