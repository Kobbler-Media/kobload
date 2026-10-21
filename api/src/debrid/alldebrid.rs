//! AllDebrid provider implementation.
//!
//! API docs: https://docs.alldebrid.com/
//! Auth: `apikey` query parameter
//! Rate limit: ~600 requests/minute

use async_trait::async_trait;
use reqwest::Client;
use serde::Deserialize;
use tracing::warn;

use super::provider::DebridProvider;
use super::types::{
    CachedAvailability, DebridAccountStatus, DebridError, DebridFile, DebridProviderType,
    DebridTorrent, DebridTorrentStatus, UnrestrictedLink,
};

const BASE_URL: &str = "https://api.alldebrid.com/v4";

pub struct AllDebridProvider;

// ── API response types ──────────────────────────────────────────────────

/// AllDebrid wraps all responses in `{ "status": "success"|"error", "data": ... }`.
#[derive(Debug, Deserialize)]
struct AdResponse<T> {
    status: String,
    data: Option<T>,
    error: Option<AdError>,
}

#[derive(Debug, Deserialize)]
struct AdError {
    code: Option<String>,
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct AdAvailabilityData {
    magnets: Vec<AdAvailabilityMagnet>,
}

#[derive(Debug, Deserialize)]
struct AdAvailabilityMagnet {
    hash: String,
    instant: bool,
    #[serde(default)]
    files: Vec<AdAvailabilityFile>,
}

#[derive(Debug, Deserialize)]
struct AdAvailabilityFile {
    #[serde(rename = "n")]
    name: String,
    #[serde(rename = "s")]
    size: Option<i64>,
    /// Subdirectory entries (AllDebrid nests directories).
    #[serde(rename = "e", default)]
    entries: Vec<AdAvailabilityFile>,
}

#[derive(Debug, Deserialize)]
struct AdMagnetUploadData {
    magnets: Vec<AdMagnetUploadResult>,
}

#[derive(Debug, Deserialize)]
struct AdMagnetUploadResult {
    id: Option<i64>,
    error: Option<AdError>,
}

#[derive(Debug, Deserialize)]
struct AdMagnetStatusData {
    magnets: AdMagnetStatus,
}

#[derive(Debug, Deserialize)]
struct AdMagnetStatus {
    id: i64,
    status: String,
    #[serde(rename = "statusCode")]
    status_code: i32,
    #[serde(default)]
    links: Vec<AdMagnetLink>,
}

#[derive(Debug, Deserialize)]
struct AdMagnetLink {
    link: String,
    filename: String,
    size: i64,
}

#[derive(Debug, Deserialize)]
struct AdUnlockData {
    link: String,
    filename: String,
    filesize: i64,
}

#[derive(Debug, Deserialize)]
struct AdUserData {
    user: AdUser,
}

#[derive(Debug, Deserialize)]
struct AdUser {
    username: String,
    #[serde(rename = "isPremium")]
    is_premium: bool,
    #[serde(rename = "premiumUntil")]
    premium_until: Option<i64>,
}

// ── Helpers ─────────────────────────────────────────────────────────────

fn api_url(path: &str, api_key: &str) -> String {
    let separator = if path.contains('?') { '&' } else { '?' };
    format!("{BASE_URL}{path}{separator}apikey={api_key}")
}

async fn check_ad_response<T: serde::de::DeserializeOwned>(
    resp: reqwest::Response,
) -> Result<T, DebridError> {
    let status = resp.status();

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

    if !status.is_success() {
        let body = resp.text().await.unwrap_or_default();
        return Err(DebridError::Status(status, body));
    }

    let body = resp.text().await.unwrap_or_default();
    let wrapper: AdResponse<T> = serde_json::from_str(&body)
        .map_err(|e| DebridError::Parse(format!("Failed to parse AllDebrid response: {e}")))?;

    if wrapper.status == "error" {
        let msg = wrapper.error.and_then(|e| {
            let code = e.code.unwrap_or_default();
            let message = e.message.unwrap_or_default();
            if code == "AUTH_BAD_APIKEY" || code == "AUTH_MISSING_APIKEY" {
                return None; // Signal auth failure below
            }
            Some(format!("{code}: {message}"))
        });

        // Check for auth errors by code
        if msg.is_none() {
            return Err(DebridError::AuthFailed);
        }

        return Err(DebridError::Other(msg.unwrap_or_default()));
    }

    wrapper
        .data
        .ok_or_else(|| DebridError::Parse("AllDebrid response missing 'data' field".to_string()))
}

/// Flatten AllDebrid's nested file tree into a flat list with paths.
fn flatten_files(
    files: &[AdAvailabilityFile],
    prefix: &str,
    out: &mut Vec<DebridFile>,
    id: &mut i32,
) {
    for file in files {
        let path = if prefix.is_empty() {
            file.name.clone()
        } else {
            format!("{prefix}/{}", file.name)
        };

        if file.entries.is_empty() {
            // Leaf file
            *id += 1;
            out.push(DebridFile {
                id: *id,
                path,
                size: file.size.unwrap_or(0),
                selected: false,
            });
        } else {
            // Directory — recurse
            flatten_files(&file.entries, &path, out, id);
        }
    }
}

// ── DebridProvider implementation ───────────────────────────────────────

#[async_trait]
impl DebridProvider for AllDebridProvider {
    fn provider_type(&self) -> DebridProviderType {
        DebridProviderType::AllDebrid
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

        // Build URL with magnets[] query params
        let mut url = api_url("/magnet/instant?", api_key);
        // Remove trailing '&' from api_url since path already has '?'
        for hash in hashes {
            url.push_str(&format!("&magnets[]={hash}"));
        }

        let resp = http_client.get(&url).send().await?;
        let data: AdAvailabilityData = check_ad_response(resp).await?;

        let mut results = Vec::new();
        for magnet in data.magnets {
            if !magnet.instant {
                continue;
            }

            let mut id_counter = 0;
            let mut files = Vec::new();
            flatten_files(&magnet.files, "", &mut files, &mut id_counter);

            if !files.is_empty() {
                results.push(CachedAvailability {
                    hash: magnet.hash,
                    available: true,
                    file_variants: vec![files],
                });
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
        let url = api_url("/magnet/upload", api_key);
        let resp = http_client
            .post(&url)
            .form(&[("magnets[]", magnet)])
            .send()
            .await?;

        let data: AdMagnetUploadData = check_ad_response(resp).await?;
        let result = data.magnets.into_iter().next().ok_or_else(|| {
            DebridError::Parse("AllDebrid magnet upload returned empty result".to_string())
        })?;

        if let Some(err) = result.error {
            let msg = err.message.unwrap_or_else(|| "Unknown error".to_string());
            return Err(DebridError::Other(msg));
        }

        let id = result.id.ok_or_else(|| {
            DebridError::Parse("AllDebrid magnet upload returned no ID".to_string())
        })?;

        Ok(id.to_string())
    }

    async fn select_files(
        &self,
        _http_client: &Client,
        _api_key: &str,
        _torrent_id: &str,
        _file_ids: &[i32],
    ) -> Result<(), DebridError> {
        // AllDebrid auto-selects all files, no separate select step needed.
        Ok(())
    }

    async fn get_torrent(
        &self,
        http_client: &Client,
        api_key: &str,
        torrent_id: &str,
    ) -> Result<DebridTorrent, DebridError> {
        let url = api_url(&format!("/magnet/status?id={torrent_id}"), api_key);
        let resp = http_client.get(&url).send().await?;
        let data: AdMagnetStatusData = check_ad_response(resp).await?;

        let magnet = data.magnets;

        // AllDebrid status_code: 0=processing, 1=uploading, 2=ready, 3=uploading, 4=ready
        // Status strings: "Downloading", "Ready", "Processing", "Uploading", etc.
        let status = match magnet.status_code {
            4 => DebridTorrentStatus::Ready,
            0..=3 => DebridTorrentStatus::Downloading { progress: 0.0 },
            _ => {
                if magnet.status.to_lowercase().contains("error") {
                    DebridTorrentStatus::Error {
                        message: format!("Torrent status: {}", magnet.status),
                    }
                } else {
                    warn!(
                        status = magnet.status,
                        status_code = magnet.status_code,
                        "Unknown AllDebrid torrent status"
                    );
                    DebridTorrentStatus::Downloading { progress: 0.0 }
                }
            }
        };

        let files: Vec<DebridFile> = magnet
            .links
            .iter()
            .enumerate()
            .map(|(i, link)| DebridFile {
                id: (i + 1) as i32,
                path: link.filename.clone(),
                size: link.size,
                selected: true,
            })
            .collect();

        let links: Vec<String> = magnet.links.into_iter().map(|l| l.link).collect();

        Ok(DebridTorrent {
            id: magnet.id.to_string(),
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
        let url = api_url("/link/unlock", api_key);
        let resp = http_client
            .post(&url)
            .form(&[("link", link)])
            .send()
            .await?;

        let data: AdUnlockData = check_ad_response(resp).await?;

        Ok(UnrestrictedLink {
            url: data.link,
            filename: data.filename,
            filesize: data.filesize,
            mime_type: None,
            expires_at: None,
        })
    }

    async fn get_account_status(
        &self,
        http_client: &Client,
        api_key: &str,
    ) -> Result<DebridAccountStatus, DebridError> {
        let url = api_url("/user", api_key);
        let resp = http_client.get(&url).send().await?;
        let data: AdUserData = check_ad_response(resp).await?;

        let expiration = data.user.premium_until.and_then(|ts| {
            time::OffsetDateTime::from_unix_timestamp(ts)
                .ok()
                .and_then(|dt| {
                    dt.format(&time::format_description::well_known::Rfc3339)
                        .ok()
                })
        });

        Ok(DebridAccountStatus {
            provider: "AllDebrid".to_string(),
            username: Some(data.user.username),
            expiration,
            premium: data.user.is_premium,
            remaining_traffic: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_provider_type() {
        let provider = AllDebridProvider;
        assert_eq!(provider.provider_type(), DebridProviderType::AllDebrid);
        assert_eq!(provider.max_hashes_per_batch(), 100);
        assert!(!provider.supports_usenet());
    }

    #[test]
    fn test_api_url_no_query() {
        let url = api_url("/user", "my_key");
        assert_eq!(url, "https://api.alldebrid.com/v4/user?apikey=my_key");
    }

    #[test]
    fn test_api_url_with_query() {
        let url = api_url("/magnet/status?id=123", "my_key");
        assert_eq!(
            url,
            "https://api.alldebrid.com/v4/magnet/status?id=123&apikey=my_key"
        );
    }

    #[test]
    fn test_flatten_files_simple() {
        let files = vec![AdAvailabilityFile {
            name: "movie.mkv".to_string(),
            size: Some(5_000_000_000),
            entries: vec![],
        }];
        let mut out = Vec::new();
        let mut id = 0;
        flatten_files(&files, "", &mut out, &mut id);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].path, "movie.mkv");
        assert_eq!(out[0].size, 5_000_000_000);
        assert_eq!(out[0].id, 1);
    }

    #[test]
    fn test_flatten_files_nested() {
        let files = vec![AdAvailabilityFile {
            name: "Show".to_string(),
            size: None,
            entries: vec![
                AdAvailabilityFile {
                    name: "S01E01.mkv".to_string(),
                    size: Some(500_000_000),
                    entries: vec![],
                },
                AdAvailabilityFile {
                    name: "S01E02.mkv".to_string(),
                    size: Some(600_000_000),
                    entries: vec![],
                },
            ],
        }];
        let mut out = Vec::new();
        let mut id = 0;
        flatten_files(&files, "", &mut out, &mut id);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].path, "Show/S01E01.mkv");
        assert_eq!(out[1].path, "Show/S01E02.mkv");
    }
}
