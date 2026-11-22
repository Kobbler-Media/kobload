use std::path::PathBuf;
use tokio::fs;
use uuid::Uuid;

use super::{ScannedFile, get_extension};

pub struct ScanResult {
    pub files: Vec<ScannedFile>,
    pub error: Option<String>,
}

pub async fn scan_storage_path(storage: &str) -> ScanResult {
    if storage.is_empty() {
        return ScanResult {
            files: Vec::new(),
            error: Some("download client reported an empty storage path".to_string()),
        };
    }

    let root_path = PathBuf::from(storage);
    let mut files = Vec::new();

    match fs::metadata(&root_path).await {
        Ok(meta) if meta.is_file() => {
            let path_str = root_path.to_string_lossy().to_string();
            let filename = root_path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            files.push(ScannedFile {
                id: Uuid::now_v7(),
                absolute_path: path_str,
                relative_path: filename.clone(),
                size: meta.len() as i64,
                extension: get_extension(&filename),
                split_rar_volumes: None,
                distributed_rar_sources: None,
                distributed_numeric_sources: None,
                numeric_split_type: None,
                score: None,
                classification: None,
                file_metadata: None,
                mime_type: None,
            });
            return ScanResult { files, error: None };
        }
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => {
            return ScanResult {
                files: Vec::new(),
                error: Some(format!(
                    "Storage path is neither file nor directory: {}",
                    storage
                )),
            };
        }
        Err(e) => {
            return ScanResult {
                files: Vec::new(),
                error: Some(format!(
                    "Cannot access storage path '{}': {} (network share not mounted?)",
                    storage, e
                )),
            };
        }
    }

    let mut dirs = vec![(root_path.clone(), PathBuf::new())];
    while let Some((dir, relative_base)) = dirs.pop() {
        let mut entries = match fs::read_dir(&dir).await {
            Ok(entries) => entries,
            Err(e) => {
                if dir == root_path {
                    return ScanResult {
                        files: Vec::new(),
                        error: Some(format!(
                            "Cannot read storage directory '{}': {} (network share not mounted?)",
                            storage, e
                        )),
                    };
                }
                continue;
            }
        };

        while let Ok(Some(entry)) = entries.next_entry().await {
            let entry_path = entry.path();
            let entry_name = entry.file_name().to_string_lossy().to_string();
            let relative_path = if relative_base.as_os_str().is_empty() {
                PathBuf::from(&entry_name)
            } else {
                relative_base.join(&entry_name)
            };

            match entry.metadata().await {
                Ok(meta) if meta.is_dir() => {
                    dirs.push((entry_path, relative_path));
                }
                Ok(meta) if meta.is_file() => {
                    let absolute_str = entry_path.to_string_lossy().to_string();
                    let relative_str = relative_path.to_string_lossy().to_string();
                    files.push(ScannedFile {
                        id: Uuid::now_v7(),
                        absolute_path: absolute_str,
                        relative_path: relative_str.clone(),
                        size: meta.len() as i64,
                        extension: get_extension(&relative_str),
                        split_rar_volumes: None,
                        distributed_rar_sources: None,
                        distributed_numeric_sources: None,
                        numeric_split_type: None,
                        score: None,
                        classification: None,
                        file_metadata: None,
                        mime_type: None,
                    });
                }
                _ => {}
            }
        }
    }

    ScanResult { files, error: None }
}
