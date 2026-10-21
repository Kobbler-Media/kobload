//! Premiumize provider implementation.
//!
//! API docs: https://www.premiumize.me/api
//! Auth: Bearer token
//! Rate limit: ~300 requests/minute

use async_trait::async_trait;
use reqwest::Client;
use serde::Deserialize;
use tracing::warn;

use super::provider::DebridProvider;
use super::types::{
    CachedAvailability, DebridAccountStatus, DebridError, DebridFile, DebridProviderType,
    DebridTorrent, DebridTorrentStatus, UnrestrictedLink,
};

const BASE_URL: &str = "https://www.premiumize.me/api";

pub struct PremiumizeProvider;

// ── API response types ──────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct PmCacheCheckResponse {
    #[allow(dead_code)]
    status: String,
    response: Vec<bool>,
    #[allow(dead_code)]
    filename: Vec<Option<String>>,
    #[allow(dead_code)]
    filesize: Vec<Option<serde_json::Value>>,
}

#[derive(Debug, Deserialize)]
struct PmTransferCreateResponse {
    #[allow(dead_code)]
    status: String,
    id: String,
}

#[derive(Debug, Deserialize)]
struct PmTransferListResponse {
    #[allow(dead_code)]
    status: String,
    transfers: Vec<PmTransfer>,
}

#[derive(Debug, Deserialize)]
struct PmTransfer {
    id: String,
    status: String,
    progress: Option<f64>,
    #[allow(dead_code)]
    message: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PmDirectDlResponse {
    #[allow(dead_code)]
    status: String,
    content: Vec<PmDirectDlFile>,
}

#[derive(Debug, Deserialize)]
struct PmDirectDlFile {
    path: String,
    size: i64,
    link: String,
    #[allow(dead_code)]
    stream_link: Option<String>,
}

#[derive(Debug, Deserialize)]
struct PmAccountInfoResponse {
    #[allow(dead_code)]
    status: String,
    customer_id: String,
    premium_until: Option<i64>,
    #[allow(dead_code)]
    space_used: Option<f64>,
}

#[derive(Debug, Deserialize)]
struct PmErrorResponse {
    message: Option<String>,
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
    let error_msg = serde_json::from_str::<PmErrorResponse>(&body)
        .ok()
        .and_then(|e| e.message)
        .unwrap_or(body);

    Err(DebridError::Status(status, error_msg))
}

// ── DebridProvider implementation ───────────────────────────────────────

#[async_trait]
impl DebridProvider for PremiumizeProvider {
    fn provider_type(&self) -> DebridProviderType {
        DebridProviderType::Premiumize
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

        let url = format!("{BASE_URL}/cache/check");

        // Premiumize expects items[] form fields for each hash
        let params: Vec<(&str, &str)> = hashes.iter().map(|h| ("items[]", *h)).collect();

        let resp = http_client
            .post(&url)
            .header("Authorization", auth_header(api_key))
            .form(&params)
            .send()
            .await?;

        let resp = check_error_response(resp).await?;
        let body: PmCacheCheckResponse = resp.json().await.map_err(|e| {
            DebridError::Parse(format!("Failed to parse cache check response: {e}"))
        })?;

        let mut results = Vec::new();
        for (i, &cached) in body.response.iter().enumerate() {
            if cached && let Some(&hash) = hashes.get(i) {
                // Premiumize doesn't return per-file details in cache check,
                // so we create a single variant with a placeholder file.
                // The actual files are retrieved via directdl when needed.
                let filename = body
                    .filename
                    .get(i)
                    .and_then(|f| f.clone())
                    .unwrap_or_default();
                let filesize = body
                    .filesize
                    .get(i)
                    .and_then(|v| v.as_ref())
                    .and_then(|v| match v {
                        serde_json::Value::Number(n) => n.as_i64(),
                        serde_json::Value::String(s) => s.parse::<i64>().ok(),
                        _ => None,
                    })
                    .unwrap_or(0);

                let file = DebridFile {
                    id: 0,
                    path: filename,
                    size: filesize,
                    selected: false,
                };

                results.push(CachedAvailability {
                    hash: hash.to_string(),
                    available: true,
                    file_variants: vec![vec![file]],
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
        let url = format!("{BASE_URL}/transfer/create");
        let resp = http_client
            .post(&url)
            .header("Authorization", auth_header(api_key))
            .form(&[("src", magnet)])
            .send()
            .await?;

        let resp = check_error_response(resp).await?;
        let result: PmTransferCreateResponse = resp.json().await.map_err(|e| {
            DebridError::Parse(format!("Failed to parse transfer create response: {e}"))
        })?;

        Ok(result.id)
    }

    async fn select_files(
        &self,
        _http_client: &Client,
        _api_key: &str,
        _torrent_id: &str,
        _file_ids: &[i32],
    ) -> Result<(), DebridError> {
        // Premiumize doesn't have a separate file selection step.
        // All files are available via directdl.
        Ok(())
    }

    async fn get_torrent(
        &self,
        http_client: &Client,
        api_key: &str,
        torrent_id: &str,
    ) -> Result<DebridTorrent, DebridError> {
        let url = format!("{BASE_URL}/transfer/list");
        let resp = http_client
            .get(&url)
            .header("Authorization", auth_header(api_key))
            .send()
            .await?;

        let resp = check_error_response(resp).await?;
        let list: PmTransferListResponse = resp
            .json()
            .await
            .map_err(|e| DebridError::Parse(format!("Failed to parse transfer list: {e}")))?;

        let transfer = list
            .transfers
            .into_iter()
            .find(|t| t.id == torrent_id)
            .ok_or(DebridError::NotFound)?;

        let status = match transfer.status.as_str() {
            "finished" => DebridTorrentStatus::Ready,
            "waiting" | "queued" | "running" => DebridTorrentStatus::Downloading {
                progress: transfer.progress.unwrap_or(0.0) as f32,
            },
            "error" | "deleted" | "timeout" => DebridTorrentStatus::Error {
                message: format!("Transfer status: {}", transfer.status),
            },
            other => {
                warn!(status = other, "Unknown Premiumize transfer status");
                DebridTorrentStatus::Downloading {
                    progress: transfer.progress.unwrap_or(0.0) as f32,
                }
            }
        };

        Ok(DebridTorrent {
            id: transfer.id,
            status,
            files: vec![],
            links: vec![],
        })
    }

    async fn unrestrict_link(
        &self,
        http_client: &Client,
        api_key: &str,
        link: &str,
    ) -> Result<UnrestrictedLink, DebridError> {
        let url = format!("{BASE_URL}/transfer/directdl");
        let resp = http_client
            .post(&url)
            .header("Authorization", auth_header(api_key))
            .form(&[("src", link)])
            .send()
            .await?;

        let resp = check_error_response(resp).await?;
        let result: PmDirectDlResponse = resp
            .json()
            .await
            .map_err(|e| DebridError::Parse(format!("Failed to parse directdl response: {e}")))?;

        let file = result
            .content
            .into_iter()
            .max_by_key(|f| f.size)
            .ok_or_else(|| DebridError::Other("No files in directdl response".to_string()))?;

        Ok(UnrestrictedLink {
            url: file.link,
            filename: file.path,
            filesize: file.size,
            mime_type: None,
            expires_at: None,
        })
    }

    async fn get_account_status(
        &self,
        http_client: &Client,
        api_key: &str,
    ) -> Result<DebridAccountStatus, DebridError> {
        let url = format!("{BASE_URL}/account/info");
        let resp = http_client
            .get(&url)
            .header("Authorization", auth_header(api_key))
            .send()
            .await?;

        let resp = check_error_response(resp).await?;
        let info: PmAccountInfoResponse = resp
            .json()
            .await
            .map_err(|e| DebridError::Parse(format!("Failed to parse account info: {e}")))?;

        let expiration = info.premium_until.and_then(|ts| {
            time::OffsetDateTime::from_unix_timestamp(ts)
                .ok()
                .and_then(|dt| {
                    dt.format(&time::format_description::well_known::Rfc3339)
                        .ok()
                })
        });

        let premium = info
            .premium_until
            .map(|ts| ts > time::OffsetDateTime::now_utc().unix_timestamp())
            .unwrap_or(false);

        Ok(DebridAccountStatus {
            provider: "Premiumize".to_string(),
            username: Some(info.customer_id),
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
        let provider = PremiumizeProvider;
        assert_eq!(provider.provider_type(), DebridProviderType::Premiumize);
        assert_eq!(provider.max_hashes_per_batch(), 100);
        assert!(!provider.supports_usenet());
        assert_eq!(provider.display_name(), "Premiumize");
    }
}
