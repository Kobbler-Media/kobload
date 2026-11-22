use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobRequest {
    pub id: String,
    pub name: String,
    pub category: String,
    pub nzb_base64: String,
    #[serde(default)]
    pub password: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Downloading,
    Verifying,
    Repairing,
    Unpacking,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobFailure {
    Content,
    Infrastructure,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JobStatus {
    pub id: String,
    pub name: String,
    pub state: JobState,
    pub progress: f64,
    pub total_bytes: u64,
    pub downloaded_bytes: u64,
    #[serde(default)]
    pub storage: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub failure: Option<JobFailure>,
}
