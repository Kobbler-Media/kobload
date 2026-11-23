use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EngineActivity {
    Idle,
    Downloading,
    Streaming,
    DownloadsWaitingForStream,
    PausedLowDisk,
    ProviderFailing { host: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobPhase {
    Queued,
    Downloading,
    Verifying,
    Repairing,
    Unpacking,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitReason {
    Stream,
    LowDisk,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StreamKind {
    Playing,
    CachingAhead,
    Preloading,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderActivity {
    pub host: String,
    pub in_use: usize,
    pub open: usize,
    pub max: usize,
    pub failing: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobActivity {
    pub id: String,
    pub phase: JobPhase,
    pub downloaded_bytes: u64,
    pub total_bytes: u64,
    pub speed_bps: Option<u64>,
    pub eta_seconds: Option<u64>,
    pub waiting: Option<WaitReason>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamActivity {
    pub id: u64,
    pub name: String,
    pub kind: StreamKind,
    pub position_percent: Option<u8>,
    pub speed_bps: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineStatus {
    pub activity: EngineActivity,
    pub speed_bps: Option<u64>,
    pub providers: Vec<ProviderActivity>,
    pub jobs: Vec<JobActivity>,
    pub streams: Vec<StreamActivity>,
}
