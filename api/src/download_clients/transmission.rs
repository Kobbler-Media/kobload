use axum::http::StatusCode;
use base64::{Engine as _, engine::general_purpose};
use serde_json::json;
use std::path::Path;
use uuid::Uuid;

use crate::download_clients::{DownloadClientError, PollResult, ScannedFile, get_extension};
use crate::types::{DownloadClientConfig, TestConnectionResult};

/// Transmission seed ratio/idle modes.
/// See: https://github.com/transmission/transmission/blob/main/docs/rpc-spec.md
mod seed_mode {
    /// Use the torrent-specific limit, overriding global settings.
    pub const SINGLE: i32 = 1;
    // Other modes (for reference):
    // pub const GLOBAL: i32 = 0;     // Follow global settings
    // pub const UNLIMITED: i32 = 2;  // No limit
}

pub async fn transmission_rpc(
    http_client: &reqwest::Client,
    base_url: &str,
    username: Option<&str>,
    password: Option<&str>,
    session_id: Option<&str>,
    payload: serde_json::Value,
) -> Result<serde_json::Value, DownloadClientError> {
    let url = format!("{}/transmission/rpc", base_url.trim_end_matches('/'));
    let mut session_id = session_id.map(|value| value.to_string());

    for _ in 0..2 {
        let mut request = http_client.post(&url).json(&payload);
        if let Some(ref session_id) = session_id {
            request = request.header("X-Transmission-Session-Id", session_id);
        }
        if let (Some(user), Some(pass)) = (username, password) {
            request = request.basic_auth(user, Some(pass));
        }

        let resp = request.send().await.map_err(DownloadClientError::Request)?;
        if resp.status() == StatusCode::CONFLICT {
            session_id = resp
                .headers()
                .get("X-Transmission-Session-Id")
                .and_then(|val| val.to_str().ok())
                .map(|val| val.to_string());
            continue;
        }

        if !resp.status().is_success() {
            return Err(DownloadClientError::Status(resp.status()));
        }

        return resp.json().await.map_err(DownloadClientError::Request);
    }

    Err(DownloadClientError::Message(
        "transmission session negotiation failed".to_string(),
    ))
}

fn parse_transmission_add_response(
    response: serde_json::Value,
) -> Result<String, DownloadClientError> {
    let arguments = response.get("arguments").ok_or_else(|| {
        DownloadClientError::Message("transmission missing arguments".to_string())
    })?;
    let torrent = arguments
        .get("torrent-added")
        .or_else(|| arguments.get("torrent-duplicate"))
        .ok_or_else(|| DownloadClientError::Message("transmission missing torrent".to_string()))?;
    let hash = torrent
        .get("hashString")
        .and_then(|v| v.as_str())
        .ok_or_else(|| DownloadClientError::Message("transmission missing hash".to_string()))?;
    Ok(hash.to_string())
}

pub async fn add_transmission_bytes(
    http_client: &reqwest::Client,
    client: &DownloadClientConfig,
    bytes: &[u8],
) -> Result<String, DownloadClientError> {
    let metainfo = general_purpose::STANDARD.encode(bytes);
    let mut args = json!({ "metainfo": metainfo });
    if let Some(dir) = client.download_dir.as_ref() {
        args["download-dir"] = json!(dir);
    }

    let payload = json!({
        "method": "torrent-add",
        "arguments": args
    });

    let response = transmission_rpc(
        http_client,
        &client.base_url,
        client.username.as_deref(),
        client.password.as_deref(),
        None,
        payload,
    )
    .await?;

    let hash = parse_transmission_add_response(response)?;

    if client.seed_ratio.is_some() || client.seed_time_minutes.is_some() {
        let mut args = json!({ "ids": [hash.clone()] });
        if let Some(seed_ratio) = client.seed_ratio {
            args["seedRatioLimit"] = json!(seed_ratio);
            args["seedRatioMode"] = json!(seed_mode::SINGLE);
        }
        if let Some(seed_time_minutes) = client.seed_time_minutes {
            args["seedIdleLimit"] = json!(seed_time_minutes);
            args["seedIdleMode"] = json!(seed_mode::SINGLE);
        }
        let payload = json!({ "method": "torrent-set", "arguments": args });
        let _ = transmission_rpc(
            http_client,
            &client.base_url,
            client.username.as_deref(),
            client.password.as_deref(),
            None,
            payload,
        )
        .await;
    }

    Ok(hash)
}

pub async fn add_transmission_url(
    http_client: &reqwest::Client,
    client: &DownloadClientConfig,
    url: &str,
) -> Result<String, DownloadClientError> {
    let mut args = json!({ "filename": url });
    if let Some(dir) = client.download_dir.as_ref() {
        args["download-dir"] = json!(dir);
    }

    let payload = json!({
        "method": "torrent-add",
        "arguments": args
    });

    let response = transmission_rpc(
        http_client,
        &client.base_url,
        client.username.as_deref(),
        client.password.as_deref(),
        None,
        payload,
    )
    .await?;

    let hash = parse_transmission_add_response(response)?;

    if client.seed_ratio.is_some() || client.seed_time_minutes.is_some() {
        let mut args = json!({ "ids": [hash.clone()] });
        if let Some(seed_ratio) = client.seed_ratio {
            args["seedRatioLimit"] = json!(seed_ratio);
            args["seedRatioMode"] = json!(seed_mode::SINGLE);
        }
        if let Some(seed_time_minutes) = client.seed_time_minutes {
            args["seedIdleLimit"] = json!(seed_time_minutes);
            args["seedIdleMode"] = json!(seed_mode::SINGLE);
        }
        let payload = json!({ "method": "torrent-set", "arguments": args });
        let _ = transmission_rpc(
            http_client,
            &client.base_url,
            client.username.as_deref(),
            client.password.as_deref(),
            None,
            payload,
        )
        .await;
    }

    Ok(hash)
}

pub async fn poll_transmission(
    http_client: &reqwest::Client,
    client: &DownloadClientConfig,
    hash: &str,
) -> Result<PollResult, DownloadClientError> {
    let payload = json!({
        "method": "torrent-get",
        "arguments": {
            "fields": [
                "hashString",
                "status",
                "percentDone",
                "downloadDir",
                "files",
                "error",
                "errorString",
                "isFinished",
                "eta",
                "rateDownload",
                "totalSize"
            ],
            "ids": [hash]
        }
    });

    let response = transmission_rpc(
        http_client,
        &client.base_url,
        client.username.as_deref(),
        client.password.as_deref(),
        None,
        payload,
    )
    .await?;

    let torrents = response
        .get("arguments")
        .and_then(|v| v.get("torrents"))
        .and_then(|v| v.as_array())
        .ok_or_else(|| DownloadClientError::Message("transmission missing torrents".to_string()))?;

    let torrent = torrents.first().ok_or_else(|| {
        DownloadClientError::Message("transmission torrent not found".to_string())
    })?;

    let percent_done = torrent
        .get("percentDone")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0)
        .clamp(0.0, 1.0);
    let progress = Some(percent_done);

    // Extract ETA and rates for normalized fields
    let eta_seconds = torrent.get("eta").and_then(|v| v.as_i64());
    let download_rate = torrent.get("rateDownload").and_then(|v| v.as_i64());
    let total_size = torrent.get("totalSize").and_then(|v| v.as_i64());

    // Calculate eta_time as UTC timestamp (client can countdown without constant updates)
    let eta_time = if let Some(secs) = eta_seconds {
        if secs >= 0 {
            Some(time::OffsetDateTime::now_utc() + time::Duration::seconds(secs))
        } else {
            None
        }
    } else {
        None
    };

    // Capture client data with normalized fields for cross-client compatibility
    // Normalized fields work across all clients
    let client_data = serde_json::json!({
        // Normalized fields (same name across all download clients)
        // eta_time is a UTC timestamp - client calculates countdown locally
        "eta_time": eta_time.and_then(|t| t.format(&time::format_description::well_known::Rfc3339).ok()),
        "download_rate": download_rate,
        "total_size": total_size,
        // Transmission-specific fields for debugging
        "hashString": hash,
        "status": torrent.get("status"),
        "percentDone": percent_done,
        "downloadDir": torrent.get("downloadDir"),
        "isFinished": torrent.get("isFinished"),
        "error": torrent.get("error"),
        "errorString": torrent.get("errorString"),
        "files": torrent.get("files"),
    });

    if torrent.get("error").and_then(|v| v.as_i64()).unwrap_or(0) > 0 {
        let message = torrent
            .get("errorString")
            .and_then(|v| v.as_str())
            .unwrap_or("transmission error");
        let mut result = PollResult::failed(message.to_string(), progress);
        result.client_data = Some(client_data);
        return Ok(result);
    }

    let is_finished = torrent
        .get("isFinished")
        .and_then(|v| v.as_bool())
        .unwrap_or(false);

    if percent_done >= 1.0 || is_finished {
        let download_dir = torrent
            .get("downloadDir")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        let files = torrent
            .get("files")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();
        let scanned_files = scan_transmission_files(download_dir, &files);
        return Ok(PollResult::completed_with_data(scanned_files, client_data));
    }

    Ok(PollResult::downloading(progress))
}

pub async fn remove_transmission(
    http_client: &reqwest::Client,
    client: &DownloadClientConfig,
    hash: &str,
    delete_data: bool,
) -> Result<(), DownloadClientError> {
    let payload = json!({
        "method": "torrent-remove",
        "arguments": {
            "ids": [hash],
            "delete-local-data": delete_data
        }
    });

    transmission_rpc(
        http_client,
        &client.base_url,
        client.username.as_deref(),
        client.password.as_deref(),
        None,
        payload,
    )
    .await?;

    Ok(())
}

/// Convert Transmission's file list to ScannedFile structs
fn scan_transmission_files(download_dir: &str, files: &[serde_json::Value]) -> Vec<ScannedFile> {
    files
        .iter()
        .filter_map(|file| {
            let name = file.get("name").and_then(|v| v.as_str())?;
            let length = file.get("length").and_then(|v| v.as_i64()).unwrap_or(0);
            let absolute_path = Path::new(download_dir)
                .join(name)
                .to_string_lossy()
                .to_string();
            let extension = get_extension(name);

            Some(ScannedFile {
                id: Uuid::now_v7(),
                absolute_path,
                relative_path: name.to_string(),
                size: length,
                extension,
                split_rar_volumes: None,
                distributed_rar_sources: None,
                distributed_numeric_sources: None,
                numeric_split_type: None,
                score: None,
                classification: None,
                file_metadata: None,
                mime_type: None,
            })
        })
        .collect()
}

pub async fn test_transmission(
    http: &reqwest::Client,
    client: &DownloadClientConfig,
) -> TestConnectionResult {
    if client.base_url.is_empty() {
        return TestConnectionResult {
            success: false,
            message: "Base URL is required".to_string(),
        };
    }

    let base = client.base_url.trim_end_matches('/');
    // If URL already ends with /rpc, use it directly; otherwise append /transmission/rpc
    let url = if base.ends_with("/rpc") {
        base.to_string()
    } else {
        format!(
            "{}/transmission/rpc",
            base.trim_end_matches("/transmission")
        )
    };

    let mut request = http
        .post(&url)
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({"method": "session-get"}));

    if let (Some(user), Some(pass)) = (&client.username, &client.password)
        && !user.is_empty()
    {
        request = request.basic_auth(user, Some(pass));
    }

    match request.send().await {
        Ok(resp) if resp.status().is_success() || resp.status().as_u16() == 409 => {
            TestConnectionResult {
                success: true,
                message: "Connected to Transmission successfully".to_string(),
            }
        }
        Ok(resp) if resp.status().as_u16() == 401 => TestConnectionResult {
            success: false,
            message: "Authentication failed - check username/password".to_string(),
        },
        Ok(resp) => TestConnectionResult {
            success: false,
            message: format!("Transmission returned status {}", resp.status()),
        },
        Err(err) => TestConnectionResult {
            success: false,
            message: format!("Failed to connect: {err}"),
        },
    }
}
