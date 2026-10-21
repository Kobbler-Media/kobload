//! TorBox provider implementation.
//!
//! API docs: https://api.torbox.app/
//! Auth: Bearer token
//! Rate limit: 60/hr for creation endpoints, generous for reads

use async_trait::async_trait;
use reqwest::Client;
use serde::Deserialize;
use std::collections::HashMap;
use tracing::warn;

use super::provider::DebridProvider;
use super::types::{
    CachedAvailability, DebridAccountStatus, DebridError, DebridFile, DebridProviderType,
    DebridTorrent, DebridTorrentStatus, UnrestrictedLink, UsenetDownload,
};

const BASE_URL: &str = "https://api.torbox.app/v1/api";

pub struct TorBoxProvider;

// ── API response types ──────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct TbResponse<T> {
    #[allow(dead_code)]
    success: bool,
    data: Option<T>,
    detail: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TbCachedEntry {
    #[allow(dead_code)]
    name: Option<String>,
    #[allow(dead_code)]
    size: Option<i64>,
    #[allow(dead_code)]
    hash: Option<String>,
    #[serde(default)]
    files: Vec<TbCachedFile>,
}

#[derive(Debug, Deserialize)]
struct TbCachedFile {
    #[allow(dead_code)]
    name: Option<String>,
    size: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct TbCreateTorrentData {
    torrent_id: i64,
    #[allow(dead_code)]
    hash: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TbTorrentInfo {
    id: i64,
    hash: Option<String>,
    #[allow(dead_code)]
    name: Option<String>,
    download_state: Option<String>,
    progress: Option<f64>,
    #[serde(default)]
    files: Vec<TbTorrentFile>,
}

#[derive(Debug, Deserialize)]
struct TbTorrentFile {
    id: Option<i64>,
    #[serde(rename = "short_name")]
    short_name: Option<String>,
    #[allow(dead_code)]
    name: Option<String>,
    size: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct TbCreateUsenetData {
    usenetdownload_id: i64,
    #[allow(dead_code)]
    hash: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TbUsenetInfo {
    id: i64,
    #[allow(dead_code)]
    hash: Option<String>,
    #[allow(dead_code)]
    name: Option<String>,
    download_state: Option<String>,
    progress: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct TbUserResponse {
    email: Option<String>,
    plan: Option<i32>,
    premium_expires_at: Option<String>,
}

// ── Helpers ─────────────────────────────────────────────────────────────

fn auth_header(api_key: &str) -> String {
    format!("Bearer {api_key}")
}

async fn check_error_response(resp: reqwest::Response) -> Result<reqwest::Response, DebridError> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }

    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(DebridError::AuthFailed);
    }

    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        let retry_after = resp
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .map(std::time::Duration::from_secs);
        return Err(DebridError::RateLimited { retry_after });
    }

    if status == reqwest::StatusCode::NOT_FOUND {
        return Err(DebridError::NotFound);
    }

    let body = resp.text().await.unwrap_or_default();
    let error_msg = serde_json::from_str::<TbResponse<serde_json::Value>>(&body)
        .ok()
        .and_then(|e| e.detail)
        .unwrap_or(body);

    Err(DebridError::Status(status, error_msg))
}

fn parse_torbox_torrent_status(state: &str, progress: Option<f64>) -> DebridTorrentStatus {
    match state {
        "completed" | "cached" => DebridTorrentStatus::Ready,
        "downloading" | "uploading" | "paused" | "stalled" | "checkingDL"
        | "checkingResumeData" | "metaDL" | "queued" => DebridTorrentStatus::Downloading {
            progress: progress.unwrap_or(0.0) as f32,
        },
        "error" | "stalledError" => DebridTorrentStatus::Error {
            message: format!("Torrent status: {state}"),
        },
        other => {
            warn!(status = other, "Unknown TorBox torrent status");
            DebridTorrentStatus::Downloading {
                progress: progress.unwrap_or(0.0) as f32,
            }
        }
    }
}

fn parse_torbox_usenet_status(state: &str, progress: Option<f64>) -> DebridTorrentStatus {
    match state {
        "completed" | "cached" => DebridTorrentStatus::Ready,
        "downloading" | "paused" | "queued" => DebridTorrentStatus::Downloading {
            progress: progress.unwrap_or(0.0) as f32,
        },
        "error" => DebridTorrentStatus::Error {
            message: format!("Usenet status: {state}"),
        },
        other => {
            warn!(status = other, "Unknown TorBox usenet status");
            DebridTorrentStatus::Downloading {
                progress: progress.unwrap_or(0.0) as f32,
            }
        }
    }
}

// ── DebridProvider implementation ───────────────────────────────────────

#[async_trait]
impl DebridProvider for TorBoxProvider {
    fn provider_type(&self) -> DebridProviderType {
        DebridProviderType::TorBox
    }

    fn supports_usenet(&self) -> bool {
        true
    }

    fn max_hashes_per_batch(&self) -> usize {
        100
    }

    async fn check_availability(
        &self,
        http_client: &Client,
        api_key: &str,
        hashes: &[&str],
    ) -> Result<Vec<CachedAvailability>, DebridError> {
        if hashes.is_empty() {
            return Ok(vec![]);
        }

        let url = format!("{BASE_URL}/torrents/checkcached");
        let body = serde_json::json!({ "hashes": hashes });

        let resp = http_client
            .post(&url)
            .header("Authorization", auth_header(api_key))
            .json(&body)
            .send()
            .await?;

        let resp = check_error_response(resp).await?;
        let wrapper: TbResponse<HashMap<String, Option<TbCachedEntry>>> =
            resp.json().await.map_err(|e| {
                DebridError::Parse(format!("Failed to parse availability response: {e}"))
            })?;

        let data = wrapper.data.unwrap_or_default();
        let mut results = Vec::new();

        for hash in hashes {
            let hash_lower = hash.to_lowercase();
            if let Some(Some(entry)) = data.get(&hash_lower) {
                let files: Vec<DebridFile> = entry
                    .files
                    .iter()
                    .enumerate()
                    .map(|(i, f)| DebridFile {
                        id: i as i32,
                        path: f.name.clone().unwrap_or_default(),
                        size: f.size.unwrap_or(0),
                        selected: false,
                    })
                    .collect();

                if !files.is_empty() {
                    results.push(CachedAvailability {
                        hash: hash.to_string(),
                        available: true,
                        file_variants: vec![files],
                    });
                }
            }
        }

        Ok(results)
    }

    async fn add_magnet(
        &self,
        http_client: &Client,
        api_key: &str,
        magnet: &str,
    ) -> Result<String, DebridError> {
        let url = format!("{BASE_URL}/torrents/createtorrent");
        let resp = http_client
            .post(&url)
            .header("Authorization", auth_header(api_key))
            .form(&[("magnet", magnet)])
            .send()
            .await?;

        let resp = check_error_response(resp).await?;
        let wrapper: TbResponse<TbCreateTorrentData> = resp.json().await.map_err(|e| {
            DebridError::Parse(format!("Failed to parse createtorrent response: {e}"))
        })?;

        let data = wrapper
            .data
            .ok_or_else(|| DebridError::Parse("Missing data in createtorrent response".into()))?;

        Ok(data.torrent_id.to_string())
    }

    async fn select_files(
        &self,
        _http_client: &Client,
        _api_key: &str,
        _torrent_id: &str,
        _file_ids: &[i32],
    ) -> Result<(), DebridError> {
        // TorBox automatically selects all files; no separate selection step needed.
        Ok(())
    }

    async fn get_torrent(
        &self,
        http_client: &Client,
        api_key: &str,
        torrent_id: &str,
    ) -> Result<DebridTorrent, DebridError> {
        let url = format!("{BASE_URL}/torrents/mylist");
        let resp = http_client
            .get(&url)
            .header("Authorization", auth_header(api_key))
            .send()
            .await?;

        let resp = check_error_response(resp).await?;
        let wrapper: TbResponse<Vec<TbTorrentInfo>> = resp
            .json()
            .await
            .map_err(|e| DebridError::Parse(format!("Failed to parse torrent list: {e}")))?;

        let torrents = wrapper.data.unwrap_or_default();
        let id_num: i64 = torrent_id
            .parse()
            .map_err(|_| DebridError::Parse(format!("Invalid torrent ID: {torrent_id}")))?;

        let info = torrents
            .into_iter()
            .find(|t| t.id == id_num)
            .ok_or(DebridError::NotFound)?;

        let status = parse_torbox_torrent_status(
            info.download_state.as_deref().unwrap_or(""),
            info.progress,
        );

        let files = info
            .files
            .into_iter()
            .map(|f| DebridFile {
                id: f.id.unwrap_or(0) as i32,
                path: f.short_name.unwrap_or_default(),
                size: f.size.unwrap_or(0),
                selected: true,
            })
            .collect();

        // TorBox doesn't provide separate download links in the list response;
        // use requestdl to get individual file links.
        let links = if matches!(status, DebridTorrentStatus::Ready) {
            vec![info.hash.unwrap_or_default()]
        } else {
            vec![]
        };

        Ok(DebridTorrent {
            id: info.id.to_string(),
            status,
            files,
            links,
        })
    }

    async fn unrestrict_link(
        &self,
        http_client: &Client,
        api_key: &str,
        link: &str,
    ) -> Result<UnrestrictedLink, DebridError> {
        // For TorBox, `link` is expected to be "torrent_id:file_id".
        let parts: Vec<&str> = link.split(':').collect();
        if parts.len() != 2 {
            return Err(DebridError::Parse(format!(
                "Expected link format 'torrent_id:file_id', got: {link}"
            )));
        }
        let torrent_id = parts[0];
        let file_id = parts[1];

        let url = format!(
            "{BASE_URL}/torrents/requestdl?token={api_key}&torrent_id={torrent_id}&file_id={file_id}"
        );

        let resp = http_client
            .get(&url)
            .header("Authorization", auth_header(api_key))
            .send()
            .await?;

        let resp = check_error_response(resp).await?;
        let wrapper: TbResponse<String> = resp
            .json()
            .await
            .map_err(|e| DebridError::Parse(format!("Failed to parse requestdl response: {e}")))?;

        let download_url = wrapper
            .data
            .ok_or_else(|| DebridError::Parse("Missing data in requestdl response".into()))?;

        Ok(UnrestrictedLink {
            url: download_url,
            filename: String::new(),
            filesize: 0,
            mime_type: None,
            expires_at: None,
        })
    }

    // ── Usenet operations ───────────────────────────────────────────────

    async fn check_usenet_cached(
        &self,
        http_client: &Client,
        api_key: &str,
        ids: &[&str],
    ) -> Result<Vec<bool>, DebridError> {
        if ids.is_empty() {
            return Ok(vec![]);
        }

        let url = format!("{BASE_URL}/usenet/checkcached");
        let body = serde_json::json!({ "hashes": ids });

        let resp = http_client
            .post(&url)
            .header("Authorization", auth_header(api_key))
            .json(&body)
            .send()
            .await?;

        let resp = check_error_response(resp).await?;
        let wrapper: TbResponse<HashMap<String, Option<serde_json::Value>>> =
            resp.json().await.map_err(|e| {
                DebridError::Parse(format!("Failed to parse usenet cache response: {e}"))
            })?;

        let data = wrapper.data.unwrap_or_default();
        let results = ids
            .iter()
            .map(|id| data.get(&id.to_lowercase()).is_some_and(|v| v.is_some()))
            .collect();

        Ok(results)
    }

    async fn add_nzb(
        &self,
        http_client: &Client,
        api_key: &str,
        nzb_url: &str,
    ) -> Result<String, DebridError> {
        let url = format!("{BASE_URL}/usenet/createusenetdownload");
        let resp = http_client
            .post(&url)
            .header("Authorization", auth_header(api_key))
            .form(&[("link", nzb_url)])
            .send()
            .await?;

        let resp = check_error_response(resp).await?;
        let wrapper: TbResponse<TbCreateUsenetData> = resp.json().await.map_err(|e| {
            DebridError::Parse(format!(
                "Failed to parse createusenetdownload response: {e}"
            ))
        })?;

        let data = wrapper.data.ok_or_else(|| {
            DebridError::Parse("Missing data in createusenetdownload response".into())
        })?;

        Ok(data.usenetdownload_id.to_string())
    }

    async fn get_usenet_download(
        &self,
        http_client: &Client,
        api_key: &str,
        id: &str,
    ) -> Result<UsenetDownload, DebridError> {
        let url = format!("{BASE_URL}/usenet/mylist");
        let resp = http_client
            .get(&url)
            .header("Authorization", auth_header(api_key))
            .send()
            .await?;

        let resp = check_error_response(resp).await?;
        let wrapper: TbResponse<Vec<TbUsenetInfo>> = resp
            .json()
            .await
            .map_err(|e| DebridError::Parse(format!("Failed to parse usenet list: {e}")))?;

        let downloads = wrapper.data.unwrap_or_default();
        let id_num: i64 = id
            .parse()
            .map_err(|_| DebridError::Parse(format!("Invalid usenet download ID: {id}")))?;

        let info = downloads
            .into_iter()
            .find(|d| d.id == id_num)
            .ok_or(DebridError::NotFound)?;

        let status =
            parse_torbox_usenet_status(info.download_state.as_deref().unwrap_or(""), info.progress);

        Ok(UsenetDownload {
            id: info.id.to_string(),
            status,
            links: vec![],
        })
    }

    // ── Account operations ──────────────────────────────────────────────

    async fn get_account_status(
        &self,
        http_client: &Client,
        api_key: &str,
    ) -> Result<DebridAccountStatus, DebridError> {
        let url = format!("{BASE_URL}/user/me");
        let resp = http_client
            .get(&url)
            .header("Authorization", auth_header(api_key))
            .send()
            .await?;

        let resp = check_error_response(resp).await?;
        let wrapper: TbResponse<TbUserResponse> = resp
            .json()
            .await
            .map_err(|e| DebridError::Parse(format!("Failed to parse user response: {e}")))?;

        let user = wrapper
            .data
            .ok_or_else(|| DebridError::Parse("Missing data in user response".into()))?;

        let expiration = user.premium_expires_at.as_deref().and_then(|s| {
            time::OffsetDateTime::parse(s, &time::format_description::well_known::Iso8601::DEFAULT)
                .ok()
                .and_then(|dt| {
                    dt.format(&time::format_description::well_known::Rfc3339)
                        .ok()
                })
        });

        // TorBox plan: 0 = free, 1 = essential, 2 = pro, 3 = standard
        let premium = user.plan.unwrap_or(0) > 0;

        Ok(DebridAccountStatus {
            provider: "TorBox".to_string(),
            username: user.email,
            expiration,
            premium,
            remaining_traffic: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_provider_type() {
        let provider = TorBoxProvider;
        assert_eq!(provider.provider_type(), DebridProviderType::TorBox);
        assert!(provider.supports_usenet());
        assert_eq!(provider.max_hashes_per_batch(), 100);
    }

    #[test]
    fn test_torrent_status_parsing() {
        assert!(matches!(
            parse_torbox_torrent_status("completed", None),
            DebridTorrentStatus::Ready
        ));
        assert!(matches!(
            parse_torbox_torrent_status("cached", None),
            DebridTorrentStatus::Ready
        ));
        assert!(matches!(
            parse_torbox_torrent_status("downloading", Some(0.5)),
            DebridTorrentStatus::Downloading { progress } if (progress - 0.5).abs() < f32::EPSILON
        ));
        assert!(matches!(
            parse_torbox_torrent_status("error", None),
            DebridTorrentStatus::Error { .. }
        ));
    }

    #[test]
    fn test_usenet_status_parsing() {
        assert!(matches!(
            parse_torbox_usenet_status("completed", None),
            DebridTorrentStatus::Ready
        ));
        assert!(matches!(
            parse_torbox_usenet_status("downloading", Some(0.3)),
            DebridTorrentStatus::Downloading { progress } if (progress - 0.3).abs() < f32::EPSILON
        ));
        assert!(matches!(
            parse_torbox_usenet_status("error", None),
            DebridTorrentStatus::Error { .. }
        ));
    }
}
