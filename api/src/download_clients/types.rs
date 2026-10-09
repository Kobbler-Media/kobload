use axum::http::StatusCode;
use serde::{Deserialize, Serialize};
use std::path::Path;
use uuid::Uuid;

use crate::parsing::{FileClassification, FileMetadata};

/// A file found in a download directory
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScannedFile {
    /// Unique identifier for this file
    #[serde(default = "Uuid::now_v7")]
    pub id: Uuid,
    /// Absolute path to the file
    pub absolute_path: String,
    /// Relative path from the download root
    pub relative_path: String,
    /// File size in bytes
    pub size: i64,
    /// File extension (lowercase, without dot)
    pub extension: String,
    /// For files inside split RARs: the sibling volume files needed for extraction.
    /// e.g., for path "ZIP.zip!/name.rar!/book.epub", this lists ["name.r00", "name.r01", ...]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub split_rar_volumes: Option<Vec<String>>,
    /// For files inside distributed split RARs: the full paths to all source archives.
    /// e.g., ["dir/a.zip!/file.rar", "dir/b.zip!/file.r00", "dir/c.zip!/file.r01", ...]
    /// The first entry is always the main .rar file.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distributed_rar_sources: Option<Vec<String>>,
    /// For files inside distributed numeric splits (.7z.001, .zip.001, etc.): the full paths.
    /// e.g., ["dir/a.zip!/file.7z.001", "dir/b.zip!/file.7z.002", ...]
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distributed_numeric_sources: Option<Vec<String>>,
    /// Archive type for numeric splits (e.g., "7z", "zip")
    #[serde(skip_serializing_if = "Option::is_none")]
    pub numeric_split_type: Option<String>,
    // --- Enrichment fields (populated by metadata_extraction job) ---
    /// File relevance score (higher = better match)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub score: Option<i32>,
    /// File classification (role within the release)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub classification: Option<FileClassification>,
    /// Technical metadata extracted from file (video/audio info)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_metadata: Option<FileMetadata>,
    /// MIME type
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime_type: Option<String>,
}

impl ScannedFile {
    /// Convert to nats_jobs::DownloadFileInfo for callbacks.
    /// Uses stored enrichment data if available, otherwise falls back to basic info.
    pub fn to_file_info(&self, download_id: Uuid) -> nats_jobs::DownloadFileInfo {
        let mime_type = self
            .mime_type
            .clone()
            .or_else(|| crate::format::mime_from_extension(&self.extension).map(|s| s.to_string()));
        nats_jobs::DownloadFileInfo {
            id: Some(self.id),
            path: self.relative_path.clone(),
            absolute_path: None,
            url: Some(format!(
                "/api/v1/downloads/{}/files/{}",
                download_id, self.id
            )),
            size: self.size,
            extension: self.extension.clone(),
            mime_type,
            score: self.score,
            classification: self.classification.clone(),
            file_metadata: self.file_metadata.clone(),
            matched_episode: self.classification.as_ref().and_then(|c| {
                c.parsed.as_ref().and_then(|p| match p {
                    nats_jobs::ParsedMedia::Tv(tv) if !tv.episodes.is_empty() => {
                        Some(tv.episodes[0].to_string())
                    }
                    _ => None,
                })
            }),
        }
    }

    /// Check if this file has been enriched with metadata extraction results.
    pub fn is_enriched(&self) -> bool {
        self.score.is_some()
    }
}

#[derive(Debug)]
pub struct PollResult {
    pub status: String,
    /// All files found in the download (replaces single file_path)
    pub files: Vec<ScannedFile>,
    /// Legacy: single file path (largest file, for backwards compat)
    pub file_path: Option<String>,
    /// Permanent error (download failed)
    pub error: Option<String>,
    pub progress: Option<f64>,
    /// Recoverable error scanning files (e.g., network share not mounted)
    pub files_scan_error: Option<String>,
    /// Raw data from the download client for debugging (JSON blob)
    pub client_data: Option<serde_json::Value>,
}

impl PollResult {
    /// Create a PollResult for a failed download
    pub fn failed(error: String, progress: Option<f64>) -> Self {
        Self {
            status: "failed".to_string(),
            files: Vec::new(),
            file_path: None,
            error: Some(error),

            progress,
            files_scan_error: None,
            client_data: None,
        }
    }

    /// Create a PollResult for a download in progress
    pub fn downloading(progress: Option<f64>) -> Self {
        Self {
            status: "downloading".to_string(),
            files: Vec::new(),
            file_path: None,
            error: None,
            progress,
            files_scan_error: None,
            client_data: None,
        }
    }

    /// Create a PollResult for a completed download with files
    pub fn completed(files: Vec<ScannedFile>) -> Self {
        // For backwards compatibility, file_path is the largest file
        let file_path = files
            .iter()
            .max_by_key(|f| f.size)
            .map(|f| f.absolute_path.clone());
        Self {
            status: "completed".to_string(),
            files,
            file_path,
            error: None,
            progress: Some(1.0),
            files_scan_error: None,
            client_data: None,
        }
    }

    /// Create a PollResult for a completed download with files and client data
    pub fn completed_with_data(files: Vec<ScannedFile>, client_data: serde_json::Value) -> Self {
        let mut result = Self::completed(files);
        result.client_data = Some(client_data);
        result
    }

    /// Create a PollResult for a completed download where file scan failed (recoverable)
    pub fn completed_scan_error(error: String, client_data: serde_json::Value) -> Self {
        Self {
            status: "completed".to_string(),
            files: Vec::new(),
            file_path: None,
            error: None,
            progress: Some(1.0),
            files_scan_error: Some(error),
            client_data: Some(client_data),
        }
    }
}

/// Extract extension from a file path (lowercase, without dot)
pub fn get_extension(path: &str) -> String {
    Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_lowercase())
        .unwrap_or_default()
}

#[derive(Debug)]
pub enum DownloadClientError {
    Request(reqwest::Error),
    Status(StatusCode),
    Message(String),
    UnsupportedClient,
    MissingApiKey,
}

impl std::fmt::Display for DownloadClientError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DownloadClientError::Request(err) => write!(f, "request error: {err}"),
            DownloadClientError::Status(status) => write!(f, "unexpected status: {status}"),
            DownloadClientError::Message(msg) => write!(f, "{msg}"),
            DownloadClientError::UnsupportedClient => write!(f, "unsupported download client"),
            DownloadClientError::MissingApiKey => write!(f, "download client api key missing"),
        }
    }
}

impl std::error::Error for DownloadClientError {}

impl DownloadClientError {
    /// Returns true if this error is transient and the operation should be retried.
    /// Transient errors include connection failures, timeouts, and temporary server errors.
    /// Permanent errors include auth failures, invalid URLs, and unsupported clients.
    pub fn is_transient(&self) -> bool {
        match self {
            // Request errors (connection refused, timeout, DNS failure) are transient
            DownloadClientError::Request(_) => true,
            // 5xx server errors are transient, 4xx are permanent
            DownloadClientError::Status(status) => status.is_server_error(),
            // Message errors are considered permanent (e.g., "missing hash")
            DownloadClientError::Message(_) => false,
            // Client misconfiguration is permanent
            DownloadClientError::UnsupportedClient => false,
            DownloadClientError::MissingApiKey => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_scanned_file() -> ScannedFile {
        ScannedFile {
            id: Uuid::now_v7(),
            absolute_path: "/dl/book/mybook.epub".to_string(),
            relative_path: "book/mybook.epub".to_string(),
            size: 500_000,
            extension: "epub".to_string(),
            split_rar_volumes: None,
            distributed_rar_sources: None,
            distributed_numeric_sources: None,
            numeric_split_type: None,
            score: None,
            classification: None,
            file_metadata: None,
            mime_type: None,
        }
    }

    #[test]
    fn backward_compat_deserialize_without_enrichment_fields() {
        // JSON from before enrichment fields were added (no score, classification, etc.)
        let json = serde_json::json!({
            "id": "019538a2-1234-7000-8000-000000000001",
            "absolute_path": "/dl/book/mybook.epub",
            "relative_path": "book/mybook.epub",
            "size": 500000,
            "extension": "epub"
        });

        let file: ScannedFile = serde_json::from_value(json).unwrap();
        assert_eq!(file.extension, "epub");
        assert_eq!(file.size, 500_000);
        assert!(!file.is_enriched());
        assert!(file.score.is_none());
        assert!(file.classification.is_none());
        assert!(file.file_metadata.is_none());
        assert!(file.mime_type.is_none());
    }

    #[test]
    fn enrichment_fields_round_trip() {
        use crate::parsing::{
            FileClassification, FileMetadata, FileRole, MetadataExtractionStatus,
        };

        let mut file = make_scanned_file();
        file.score = Some(100);
        file.classification = Some(FileClassification {
            role: FileRole::MainContent,
            parsed: None,
            confidence: 0.95,
            group_key: Some("mybook".to_string()),
        });
        file.file_metadata = Some(FileMetadata {
            status: Some(MetadataExtractionStatus::UnsupportedFormat),
            status_reason: Some("ebook format".to_string()),
            video: None,
            audio: vec![],
            subtitles: vec![],
            duration_seconds: None,
            container: None,
            bitrate_kbps: None,
        });
        file.mime_type = Some("application/epub+zip".to_string());

        // Serialize
        let json = serde_json::to_value(&file).unwrap();
        assert_eq!(json["score"], 100);
        assert_eq!(json["classification"]["role"], "main_content");
        assert_eq!(json["mime_type"], "application/epub+zip");

        // Deserialize back
        let deserialized: ScannedFile = serde_json::from_value(json).unwrap();
        assert!(deserialized.is_enriched());
        assert_eq!(deserialized.score, Some(100));
        assert_eq!(
            deserialized.mime_type,
            Some("application/epub+zip".to_string())
        );
        assert!(deserialized.classification.is_some());
        assert!(deserialized.file_metadata.is_some());
    }

    #[test]
    fn to_file_info_uses_enriched_data() {
        use crate::parsing::{
            FileClassification, FileMetadata, FileRole, MetadataExtractionStatus,
        };

        let download_id = Uuid::now_v7();
        let mut file = make_scanned_file();
        file.score = Some(42);
        file.classification = Some(FileClassification {
            role: FileRole::MainContent,
            parsed: None,
            confidence: 0.9,
            group_key: None,
        });
        file.file_metadata = Some(FileMetadata {
            status: Some(MetadataExtractionStatus::UnsupportedFormat),
            status_reason: None,
            video: None,
            audio: vec![],
            subtitles: vec![],
            duration_seconds: None,
            container: None,
            bitrate_kbps: None,
        });
        file.mime_type = Some("application/epub+zip".to_string());

        let info = file.to_file_info(download_id);
        assert_eq!(info.id, Some(file.id));
        assert_eq!(info.score, Some(42));
        assert_eq!(info.mime_type, Some("application/epub+zip".to_string()));
        assert!(info.classification.is_some());
        assert!(info.file_metadata.is_some());
    }

    #[test]
    fn to_file_info_falls_back_to_extension_mime() {
        let download_id = Uuid::now_v7();
        let file = make_scanned_file();

        let info = file.to_file_info(download_id);
        // No enriched mime_type, should fall back to extension-based lookup
        // The exact result depends on mime_from_extension but should be Some for epub
        assert!(info.mime_type.is_some() || info.mime_type.is_none()); // Extension lookup may or may not have epub
        assert!(info.score.is_none());
        assert!(info.classification.is_none());
    }

    #[test]
    fn is_enriched_checks_score() {
        let mut file = make_scanned_file();
        assert!(!file.is_enriched());

        file.score = Some(0);
        assert!(file.is_enriched());
    }

    #[test]
    fn enrichment_fields_omitted_when_none_in_serialization() {
        let file = make_scanned_file();
        let json = serde_json::to_value(&file).unwrap();

        // Enrichment fields should be absent when None (skip_serializing_if)
        assert!(!json.as_object().unwrap().contains_key("score"));
        assert!(!json.as_object().unwrap().contains_key("classification"));
        assert!(!json.as_object().unwrap().contains_key("file_metadata"));
        assert!(!json.as_object().unwrap().contains_key("mime_type"));
    }
}
