use std::path::PathBuf;
use tokio::fs;
use uuid::Uuid;

use crate::download_clients::{DownloadClientError, PollResult, ScannedFile, get_extension};
use crate::types::{DownloadClientConfig, TestConnectionResult};

/// Parse SABnzbd timeleft string (format: "H:MM:SS" or "0:02:35") to seconds.
fn parse_timeleft_to_seconds(timeleft: &str) -> Option<i64> {
    let parts: Vec<&str> = timeleft.split(':').collect();
    match parts.len() {
        3 => {
            let hours: i64 = parts[0].parse().ok()?;
            let mins: i64 = parts[1].parse().ok()?;
            let secs: i64 = parts[2].parse().ok()?;
            Some(hours * 3600 + mins * 60 + secs)
        }
        2 => {
            let mins: i64 = parts[0].parse().ok()?;
            let secs: i64 = parts[1].parse().ok()?;
            Some(mins * 60 + secs)
        }
        _ => None,
    }
}

pub async fn add_sabnzbd(
    http_client: &reqwest::Client,
    client: &DownloadClientConfig,
    bytes: &[u8],
    title: &str,
) -> Result<String, DownloadClientError> {
    let api_key = client
        .api_key
        .as_deref()
        .ok_or(DownloadClientError::MissingApiKey)?;

    let url = format!("{}/api", client.base_url.trim_end_matches('/'));

    let mut request = http_client.post(&url);
    let mut query = vec![("mode", "addfile"), ("output", "json"), ("apikey", api_key)];
    if let Some(category) = client.category.as_deref().filter(|s| !s.is_empty()) {
        query.push(("cat", category));
    }

    request = request.query(&query);

    let filename = format!(
        "{}.nzb",
        title.replace(['/', '\\', ':', '*', '?', '"', '<', '>', '|'], "_")
    );
    let part = reqwest::multipart::Part::bytes(bytes.to_vec())
        .file_name(filename)
        .mime_str("application/x-nzb")
        .map_err(|e| DownloadClientError::Message(format!("mime error: {e}")))?;
    let form = reqwest::multipart::Form::new().part("nzbfile", part);

    let resp = request
        .multipart(form)
        .send()
        .await
        .map_err(DownloadClientError::Request)?;

    if !resp.status().is_success() {
        return Err(DownloadClientError::Status(resp.status()));
    }

    let payload: serde_json::Value = resp.json().await.map_err(DownloadClientError::Request)?;

    if let Some(error) = payload
        .get("error")
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty())
    {
        return Err(DownloadClientError::Message(format!(
            "SABnzbd error: {error}"
        )));
    }

    // SABnzbd can return multiple nzo_ids when an NZB contains multiple file groups
    // (e.g. a season pack NZB with one group per episode). Join all IDs with comma.
    let id = payload
        .get("nzo_ids")
        .and_then(|v| v.as_array())
        .and_then(|arr| {
            let ids: Vec<&str> = arr.iter().filter_map(|v| v.as_str()).collect();
            if ids.is_empty() {
                None
            } else {
                Some(ids.join(","))
            }
        })
        .or_else(|| {
            payload
                .get("nzo_id")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        })
        .ok_or_else(|| {
            DownloadClientError::Message(format!("sabnzbd missing nzo_id: {payload}"))
        })?;
    Ok(id)
}

pub async fn poll_sabnzbd(
    http_client: &reqwest::Client,
    client: &DownloadClientConfig,
    nzo_id: &str,
) -> Result<PollResult, DownloadClientError> {
    if nzo_id.contains(',') {
        return poll_sabnzbd_multi(http_client, client, nzo_id).await;
    }

    let api_key = client
        .api_key
        .as_deref()
        .ok_or(DownloadClientError::MissingApiKey)?;

    let queue_json = fetch_queue(http_client, client, api_key).await?;
    if let Some(slots) = queue_json
        .get("queue")
        .and_then(|v| v.get("slots"))
        .and_then(|v| v.as_array())
        && let Some(slot) = slots
            .iter()
            .find(|slot| slot.get("nzo_id").and_then(|v| v.as_str()) == Some(nzo_id))
    {
        let progress = slot
            .get("percentage")
            .and_then(|v| v.as_str())
            .and_then(|value| value.parse::<f64>().ok())
            .map(|value| (value / 100.0).clamp(0.0, 1.0));

        // Parse timeleft string (format: "H:MM:SS" or "0:02:35") to seconds
        let eta_seconds = slot
            .get("timeleft")
            .and_then(|v| v.as_str())
            .and_then(parse_timeleft_to_seconds);

        // Calculate eta_time as UTC timestamp (client can countdown without constant updates)
        let eta_time = eta_seconds.and_then(|secs| {
            let t = time::OffsetDateTime::now_utc() + time::Duration::seconds(secs);
            t.format(&time::format_description::well_known::Rfc3339)
                .ok()
        });

        // Parse download rate from kbpersec (KB/s -> bytes/s)
        let download_rate = slot
            .get("kbpersec")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<f64>().ok())
            .map(|kbps| (kbps * 1024.0) as i64);

        // Parse total size from mb (MB -> bytes)
        let total_size = slot
            .get("mb")
            .and_then(|v| v.as_str())
            .and_then(|s| s.parse::<f64>().ok())
            .map(|mb| (mb * 1024.0 * 1024.0) as i64);

        // Build client_data with normalized fields
        let client_data = serde_json::json!({
            // Normalized fields (same name across all download clients)
            // eta_time is a UTC timestamp - client calculates countdown locally
            "eta_time": eta_time,
            "download_rate": download_rate,
            "total_size": total_size,
            // SABnzbd-specific fields for debugging
            "nzo_id": nzo_id,
            "status": slot.get("status"),
            "timeleft": slot.get("timeleft"),
            "percentage": slot.get("percentage"),
        });

        let mut result = PollResult::downloading(progress);
        result.client_data = Some(client_data);
        return Ok(result);
    }

    let history_json = fetch_history(http_client, client, api_key).await?;
    let slots = history_json
        .get("history")
        .and_then(|v| v.get("slots"))
        .and_then(|v| v.as_array())
        .ok_or_else(|| DownloadClientError::Message("sabnzbd history missing".to_string()))?;

    let slot = slots
        .iter()
        .find(|slot| slot.get("nzo_id").and_then(|v| v.as_str()) == Some(nzo_id));

    let Some(slot) = slot else {
        return Err(DownloadClientError::Message(
            "sabnzbd item not found".to_string(),
        ));
    };

    let status_text = slot
        .get("status")
        .and_then(|v| v.as_str())
        .unwrap_or("Unknown");

    // Parse total size from bytes field
    let total_size = slot.get("bytes").and_then(|v| v.as_i64());

    // Capture client data with normalized fields for cross-client compatibility
    let client_data = serde_json::json!({
        // Normalized fields (same name across all download clients)
        // For completed items, eta_time and download_rate are not applicable
        "eta_time": None::<String>,
        "download_rate": None::<i64>,
        "total_size": total_size,
        // SABnzbd-specific fields for debugging
        "nzo_id": nzo_id,
        "status": status_text,
        "storage": slot.get("storage"),
        "name": slot.get("name"),
        "category": slot.get("category"),
        "fail_message": slot.get("fail_message"),
        "completed": slot.get("completed"),
    });

    if status_text.eq_ignore_ascii_case("Completed") {
        let storage = slot.get("storage").and_then(|v| v.as_str()).unwrap_or("");
        let scan_result = scan_storage_path(storage).await;

        if let Some(scan_error) = scan_result.error {
            // Files unavailable - recoverable error (network share not mounted, etc.)
            return Ok(PollResult::completed_scan_error(scan_error, client_data));
        }

        return Ok(PollResult::completed_with_data(
            scan_result.files,
            client_data,
        ));
    }

    if status_text.eq_ignore_ascii_case("Failed") {
        let error_message = slot
            .get("fail_message")
            .and_then(|v| v.as_str())
            .unwrap_or("sabnzbd failed");
        let mut result = PollResult::failed(error_message.to_string(), None);
        result.client_data = Some(client_data);
        return Ok(result);
    }

    Ok(PollResult::downloading(None))
}

/// Poll multiple SABnzbd sub-jobs (comma-separated nzo_ids from a split NZB).
/// Fetches queue and history once, classifies each sub-job, and aggregates.
async fn poll_sabnzbd_multi(
    http_client: &reqwest::Client,
    client: &DownloadClientConfig,
    nzo_ids_str: &str,
) -> Result<PollResult, DownloadClientError> {
    let api_key = client
        .api_key
        .as_deref()
        .ok_or(DownloadClientError::MissingApiKey)?;
    let nzo_ids: Vec<&str> = nzo_ids_str.split(',').collect();

    let queue_json = fetch_queue(http_client, client, api_key).await?;
    let queue_slots = queue_json
        .get("queue")
        .and_then(|v| v.get("slots"))
        .and_then(|v| v.as_array());

    let history_json = fetch_history(http_client, client, api_key).await?;
    let history_slots = history_json
        .get("history")
        .and_then(|v| v.get("slots"))
        .and_then(|v| v.as_array());

    let mut in_queue_count: usize = 0;
    let mut queue_progress_sum: f64 = 0.0;
    let mut completed_slots: Vec<&serde_json::Value> = Vec::new();
    let mut failed_message: Option<String> = None;
    let mut not_found: Vec<&str> = Vec::new();

    for &nzo_id in &nzo_ids {
        // Check queue first
        let in_q = queue_slots.and_then(|slots| {
            slots
                .iter()
                .find(|s| s.get("nzo_id").and_then(|v| v.as_str()) == Some(nzo_id))
        });
        if let Some(slot) = in_q {
            in_queue_count += 1;
            let pct = slot
                .get("percentage")
                .and_then(|v| v.as_str())
                .and_then(|v| v.parse::<f64>().ok())
                .unwrap_or(0.0);
            queue_progress_sum += pct;
            continue;
        }

        // Check history
        let in_hist = history_slots.and_then(|slots| {
            slots
                .iter()
                .find(|s| s.get("nzo_id").and_then(|v| v.as_str()) == Some(nzo_id))
        });
        if let Some(slot) = in_hist {
            let status = slot.get("status").and_then(|v| v.as_str()).unwrap_or("");
            if status.eq_ignore_ascii_case("Completed") {
                completed_slots.push(slot);
            } else if status.eq_ignore_ascii_case("Failed") {
                if failed_message.is_none() {
                    failed_message = Some(
                        slot.get("fail_message")
                            .and_then(|v| v.as_str())
                            .unwrap_or("sabnzbd sub-job failed")
                            .to_string(),
                    );
                }
            } else {
                // Other status (e.g. Extracting, Verifying) — treat as still downloading
                in_queue_count += 1;
            }
        } else {
            not_found.push(nzo_id);
        }
    }

    let client_data = serde_json::json!({
        "nzo_ids": nzo_ids_str,
        "sub_jobs_total": nzo_ids.len(),
        "sub_jobs_in_queue": in_queue_count,
        "sub_jobs_completed": completed_slots.len(),
        "sub_jobs_failed": if failed_message.is_some() { 1 } else { 0 },
        "sub_jobs_not_found": not_found.len(),
    });

    // Priority: failed > not_found > downloading > all completed
    if let Some(msg) = failed_message {
        let mut result = PollResult::failed(msg, None);
        result.client_data = Some(client_data);
        return Ok(result);
    }

    if !not_found.is_empty() {
        return Err(DownloadClientError::Message(format!(
            "sabnzbd sub-jobs not found: {}",
            not_found.join(", ")
        )));
    }

    if in_queue_count > 0 {
        // Average progress: completed sub-jobs count as 100%
        let completed_progress = completed_slots.len() as f64 * 100.0;
        let avg = (queue_progress_sum + completed_progress) / nzo_ids.len() as f64;
        let progress = Some((avg / 100.0).clamp(0.0, 1.0));
        let mut result = PollResult::downloading(progress);
        result.client_data = Some(client_data);
        return Ok(result);
    }

    // All completed — merge files from all storage paths
    let mut all_files = Vec::new();
    let mut scan_errors = Vec::new();
    for slot in &completed_slots {
        let storage = slot.get("storage").and_then(|v| v.as_str()).unwrap_or("");
        let scan = scan_storage_path(storage).await;
        if let Some(err) = scan.error {
            scan_errors.push(err);
        } else {
            all_files.extend(scan.files);
        }
    }

    if !scan_errors.is_empty() {
        return Ok(PollResult::completed_scan_error(
            scan_errors.join("; "),
            client_data,
        ));
    }

    Ok(PollResult::completed_with_data(all_files, client_data))
}

async fn fetch_queue(
    http_client: &reqwest::Client,
    client: &DownloadClientConfig,
    api_key: &str,
) -> Result<serde_json::Value, DownloadClientError> {
    let resp = http_client
        .get(format!("{}/api", client.base_url.trim_end_matches('/')))
        .query(&[("mode", "queue"), ("output", "json"), ("apikey", api_key)])
        .send()
        .await
        .map_err(DownloadClientError::Request)?;
    if !resp.status().is_success() {
        return Err(DownloadClientError::Status(resp.status()));
    }
    resp.json().await.map_err(DownloadClientError::Request)
}

async fn fetch_history(
    http_client: &reqwest::Client,
    client: &DownloadClientConfig,
    api_key: &str,
) -> Result<serde_json::Value, DownloadClientError> {
    let resp = http_client
        .get(format!("{}/api", client.base_url.trim_end_matches('/')))
        .query(&[("mode", "history"), ("output", "json"), ("apikey", api_key)])
        .send()
        .await
        .map_err(DownloadClientError::Request)?;
    if !resp.status().is_success() {
        return Err(DownloadClientError::Status(resp.status()));
    }
    resp.json().await.map_err(DownloadClientError::Request)
}

pub async fn remove_sabnzbd(
    http_client: &reqwest::Client,
    client: &DownloadClientConfig,
    nzo_id: &str,
    delete_data: bool,
) -> Result<(), DownloadClientError> {
    let api_key = client
        .api_key
        .as_deref()
        .ok_or(DownloadClientError::MissingApiKey)?;

    let delete_flag = if delete_data { "1" } else { "0" };
    // A job lives in either the queue (still downloading/queued/paused) or the
    // history (post-processing done or failed). `history/delete` never touches
    // queue items, so send both — deleting an id a list doesn't hold is a no-op.
    // Delete each sub-job individually (handles both single and multi-nzo).
    for id in nzo_id.split(',') {
        for mode in ["queue", "history"] {
            let mut query: Vec<(&str, &str)> = vec![
                ("mode", mode),
                ("name", "delete"),
                ("value", id),
                ("del_files", delete_flag),
                ("output", "json"),
                ("apikey", api_key),
            ];
            // `archive=0` is history-only: by default deleted items are moved to
            // the archive list; 0 removes them completely. Queue ignores it.
            if mode == "history" {
                query.push(("archive", "0"));
            }
            let resp = http_client
                .get(format!("{}/api", client.base_url.trim_end_matches('/')))
                .query(&query)
                .send()
                .await
                .map_err(DownloadClientError::Request)?;

            if !resp.status().is_success() {
                return Err(DownloadClientError::Status(resp.status()));
            }

            // SABnzbd reports API errors (bad key, etc.) as HTTP 200 + {"error": ...}
            let payload: serde_json::Value =
                resp.json().await.map_err(DownloadClientError::Request)?;
            if let Some(error) = payload
                .get("error")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
            {
                return Err(DownloadClientError::Message(format!(
                    "SABnzbd {mode} delete error: {error}"
                )));
            }
        }
    }

    Ok(())
}

/// Result of scanning a storage path
pub struct ScanResult {
    pub files: Vec<ScannedFile>,
    /// If set, indicates the scan failed (directory inaccessible, etc.)
    pub error: Option<String>,
}

/// Scan storage path and return all files found
pub async fn scan_storage_path(storage: &str) -> ScanResult {
    if storage.is_empty() {
        return ScanResult {
            files: Vec::new(),
            error: Some("SABnzbd reported empty storage path".to_string()),
        };
    }

    let root_path = PathBuf::from(storage);
    let mut files = Vec::new();

    match fs::metadata(&root_path).await {
        Ok(meta) if meta.is_file() => {
            // Single file - return it directly
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
        Ok(meta) if meta.is_dir() => {
            // Directory - scan recursively (continues below)
        }
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

    // Recursively scan directory
    let mut dirs = vec![(root_path.clone(), PathBuf::new())];
    while let Some((dir, relative_base)) = dirs.pop() {
        let mut entries = match fs::read_dir(&dir).await {
            Ok(entries) => entries,
            Err(e) => {
                // If we can't read the root directory, that's an error
                if dir == root_path {
                    return ScanResult {
                        files: Vec::new(),
                        error: Some(format!(
                            "Cannot read storage directory '{}': {} (network share not mounted?)",
                            storage, e
                        )),
                    };
                }
                // For subdirectories, just skip
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

pub async fn test_sabnzbd(
    http: &reqwest::Client,
    client: &DownloadClientConfig,
) -> TestConnectionResult {
    let Some(api_key) = &client.api_key else {
        return TestConnectionResult {
            success: false,
            message: "SABnzbd API key is required".to_string(),
        };
    };

    if api_key.is_empty() {
        return TestConnectionResult {
            success: false,
            message: "SABnzbd API key is required".to_string(),
        };
    }

    let url = format!(
        "{}/api?mode=version&apikey={}&output=json",
        client.base_url.trim_end_matches('/'),
        api_key
    );

    match http.get(&url).send().await {
        Ok(resp) if resp.status().is_success() => TestConnectionResult {
            success: true,
            message: "Connected to SABnzbd successfully".to_string(),
        },
        Ok(resp) if resp.status().as_u16() == 401 || resp.status().as_u16() == 403 => {
            TestConnectionResult {
                success: false,
                message: "Authentication failed - check API key".to_string(),
            }
        }
        Ok(resp) => TestConnectionResult {
            success: false,
            message: format!("SABnzbd returned status {}", resp.status()),
        },
        Err(err) => TestConnectionResult {
            success: false,
            message: format!("Failed to connect: {err}"),
        },
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn test_parse_multi_nzo_ids_joins_with_comma() {
        let payload = serde_json::json!({
            "status": true,
            "nzo_ids": ["SABnzbd_nzo_abc123", "SABnzbd_nzo_def456", "SABnzbd_nzo_ghi789"]
        });
        let id = payload
            .get("nzo_ids")
            .and_then(|v| v.as_array())
            .and_then(|arr| {
                let ids: Vec<&str> = arr.iter().filter_map(|v| v.as_str()).collect();
                if ids.is_empty() {
                    None
                } else {
                    Some(ids.join(","))
                }
            });
        assert_eq!(
            id.as_deref(),
            Some("SABnzbd_nzo_abc123,SABnzbd_nzo_def456,SABnzbd_nzo_ghi789")
        );
    }

    #[test]
    fn test_parse_single_nzo_id_no_comma() {
        let payload = serde_json::json!({
            "status": true,
            "nzo_ids": ["SABnzbd_nzo_abc123"]
        });
        let id = payload
            .get("nzo_ids")
            .and_then(|v| v.as_array())
            .and_then(|arr| {
                let ids: Vec<&str> = arr.iter().filter_map(|v| v.as_str()).collect();
                if ids.is_empty() {
                    None
                } else {
                    Some(ids.join(","))
                }
            })
            .unwrap();
        assert!(!id.contains(','), "Single nzo_id should not contain comma");
        assert_eq!(id, "SABnzbd_nzo_abc123");
    }

    #[test]
    fn test_multi_nzo_comma_detection() {
        let single = "SABnzbd_nzo_abc123";
        let multi = "SABnzbd_nzo_abc123,SABnzbd_nzo_def456";
        assert!(!single.contains(','));
        assert!(multi.contains(','));
        assert_eq!(multi.split(',').count(), 2);
    }

    #[test]
    fn test_parse_timeleft() {
        use super::parse_timeleft_to_seconds;
        assert_eq!(parse_timeleft_to_seconds("0:02:35"), Some(155));
        assert_eq!(parse_timeleft_to_seconds("1:30:00"), Some(5400));
        assert_eq!(parse_timeleft_to_seconds("05:30"), Some(330));
    }
}
