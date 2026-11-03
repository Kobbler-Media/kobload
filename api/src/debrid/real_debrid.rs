//! Real-Debrid provider implementation.
//!
//! API docs: https://api.real-debrid.com/
//! Auth: Bearer token
//! Rate limit: 250 requests/minute
//! Availability check: up to 100 hashes per request

use std::time::Duration;

use async_trait::async_trait;
use reqwest::Client;
use serde::Deserialize;
use tracing::warn;

use super::provider::DebridProvider;
use super::rd_names;
use super::types::{
    ActiveSlots, AddPauses, CachedAvailability, DebridAccountStatus, DebridError, DebridFile,
    DebridProviderType, DebridTorrent, DebridTorrentStatus, ExistingTorrent, UnrestrictedLink,
};

const BASE_URL: &str = "https://api.real-debrid.com/rest/1.0";

pub struct RealDebridProvider;

// ── API response types ──────────────────────────────────────────────────

// The availability response is: { "hash": { "rd": [ { "file_id": { filename, filesize } } ] } }
// It's deeply nested with dynamic keys.

#[derive(Debug, Deserialize)]
struct RdAddMagnetResponse {
    id: String,
}

#[derive(Debug, Deserialize)]
struct RdTorrentInfo {
    id: String,
    status: String,
    progress: Option<f64>,
    #[serde(default)]
    files: Vec<RdTorrentFile>,
    #[serde(default)]
    links: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct RdTorrentFile {
    id: i32,
    path: String,
    bytes: i64,
    selected: Option<i32>,
}

#[derive(Debug, Deserialize)]
struct RdUnrestrictResponse {
    filename: String,
    download: String,
    filesize: i64,
    #[serde(rename = "mimeType")]
    mime_type: Option<String>,
}

/// One entry from `/torrents` (the account's torrent list).
#[derive(Debug, Deserialize)]
struct RdTorrentListItem {
    id: String,
    hash: String,
    status: String,
    #[serde(default)]
    links: Vec<String>,
}

/// `/torrents/activeCount` — current active torrents and the account limit.
#[derive(Debug, Deserialize)]
struct RdActiveCountResponse {
    nb: u32,
    limit: u32,
}

#[derive(Debug, Deserialize)]
struct RdUserResponse {
    username: String,
    #[serde(rename = "type")]
    account_type: String,
    expiration: String,
    points: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct RdErrorResponse {
    error: Option<String>,
    error_code: Option<i32>,
}

// Real-Debrid `error_code` values we act on. Full table: https://api.real-debrid.com/
/// Hoster unavailable — on unrestrict, this copy lost the file.
const RD_ERR_HOSTER_UNAVAILABLE: i32 = 19;
/// Torrent already active — no free slot, the hash itself is fine.
const RD_ERR_TORRENT_ALREADY_ACTIVE: i32 = 33;
/// Too many requests.
const RD_ERR_TOO_MANY_REQUESTS: i32 = 34;
/// Infringing file — since May 2026 applied heuristically by filename.
const RD_ERR_INFRINGING_FILE: i32 = 35;
/// Fair usage limit.
const RD_ERR_FAIR_USAGE: i32 = 36;
/// Traffic exhausted, too many active downloads, download limit: the account is capped
/// for now, the same answer for every hash.
const RD_ERR_ACCOUNT_LIMITS: [i32; 3] = [21, 23, 26];
/// Torrent too big: permanent for that hash, like a refusal.
const RD_ERR_TORRENT_TOO_BIG: i32 = 29;
/// Permission denied, hoster not available for free users: the account cannot do this.
const RD_ERR_ACCESS_DENIED: [i32; 2] = [9, 20];
/// Disabled endpoint (what /torrents/instantAvailability now returns).
const RD_ERR_DISABLED_ENDPOINT: i32 = 37;

// ── Helpers ─────────────────────────────────────────────────────────────

fn auth_header(api_key: &str) -> String {
    format!("Bearer {api_key}")
}

async fn check_error_response(resp: reqwest::Response) -> Result<reqwest::Response, DebridError> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }

    // `retry-after` must be read before the body consumes the response.
    let retry_after = resp
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok())
        .map(std::time::Duration::from_secs);

    // RD signals the interesting cases via `error_code` in the body rather than
    // the HTTP status (a 503 can be an infringing file, a fair-usage block, or a
    // genuine outage), so parse the body first and let the code win.
    let body = resp.text().await.unwrap_or_default();
    let parsed = serde_json::from_str::<RdErrorResponse>(&body).ok();
    let error_code = parsed.as_ref().and_then(|e| e.error_code);

    match error_code {
        Some(RD_ERR_INFRINGING_FILE) => return Err(DebridError::Blocked),
        Some(RD_ERR_TORRENT_ALREADY_ACTIVE) => return Err(DebridError::TooManyActive),
        Some(RD_ERR_FAIR_USAGE) => return Err(DebridError::FairUsage),
        Some(RD_ERR_HOSTER_UNAVAILABLE) => return Err(DebridError::HosterUnavailable),
        Some(RD_ERR_TORRENT_TOO_BIG) => return Err(DebridError::TooBig),
        Some(code) if RD_ERR_ACCOUNT_LIMITS.contains(&code) => {
            return Err(DebridError::FairUsage);
        }
        Some(code) if RD_ERR_ACCESS_DENIED.contains(&code) => {
            return Err(DebridError::AuthFailed);
        }
        Some(RD_ERR_TOO_MANY_REQUESTS) => return Err(DebridError::RateLimited { retry_after }),
        Some(RD_ERR_DISABLED_ENDPOINT) => {
            return Err(DebridError::UnsupportedOperation);
        }
        _ => {}
    }

    if status == reqwest::StatusCode::UNAUTHORIZED || status == reqwest::StatusCode::FORBIDDEN {
        return Err(DebridError::AuthFailed);
    }

    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        return Err(DebridError::RateLimited { retry_after });
    }

    if status == reqwest::StatusCode::NOT_FOUND {
        return Err(DebridError::NotFound);
    }

    let error_msg = parsed.and_then(|e| e.error).unwrap_or(body);

    Err(DebridError::Status(status, error_msg))
}

/// Map a Real-Debrid torrent status string to our status enum.
///
/// Shared by `/torrents/info/{id}` and the `/torrents` listing, which use the
/// same vocabulary; the listing carries no progress, hence the `Option`.
fn map_torrent_status(status: &str, progress: Option<f64>) -> DebridTorrentStatus {
    let pct = progress.unwrap_or(0.0) as f32 / 100.0;
    match status {
        // "dead" is a finished torrent whose swarm died; RD still serves its files.
        "downloaded" | "dead" => DebridTorrentStatus::Ready,
        "waiting_files_selection" => DebridTorrentStatus::WaitingFileSelection,
        "magnet_conversion" | "queued" | "downloading" | "uploading" | "compressing" => {
            DebridTorrentStatus::Downloading { progress: pct }
        }
        // "virus" is RD's status for a file the filter-gate has rejected as well
        // as for genuine malware flags; either way the hash is dead to us
        // permanently, so surface it distinctly from transient errors.
        "virus" => DebridTorrentStatus::Blocked,
        "magnet_error" | "error" => DebridTorrentStatus::Error {
            message: format!("Torrent status: {status}"),
        },
        other => {
            warn!(status = other, "Unknown Real-Debrid torrent status");
            DebridTorrentStatus::Downloading { progress: pct }
        }
    }
}

// ── DebridProvider implementation ───────────────────────────────────────

#[async_trait]
impl DebridProvider for RealDebridProvider {
    fn provider_type(&self) -> DebridProviderType {
        DebridProviderType::RealDebrid
    }

    /// Measured by Debrid Media Manager (src/utils/rdAddPause.ts and services/realDebrid.ts
    /// there, 2026-10): about four adds in three minutes trip a pause of 21 s to five
    /// minutes, during which every add answers error 35.
    fn add_pauses(&self) -> Option<AddPauses> {
        Some(AddPauses {
            hold: Duration::from_secs(30),
            burst: 4,
            spacing: Duration::from_secs(45),
            refuses_name: rd_names::refuses,
        })
    }

    fn max_hashes_per_batch(&self) -> usize {
        // Process one at a time since we use addMagnet (no batch endpoint)
        // The caller already batches and runs concurrently
        20
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

        // Real-Debrid removed /torrents/instantAvailability (error_code 37,
        // "Disabled endpoint"), so the only way to ask "is this cached" is to
        // add the magnet, look at the status, and delete it again. If status is
        // "waiting_files_selection" or "downloaded", it was already cached.
        //
        // NOTE: nothing calls this today — the stream path gets the same signal
        // as a side effect of cache warming (see services/stream/warm_cache.rs),
        // which avoids paying for a second round of add/delete churn. Kept for
        // callers that need a standalone check.
        //
        // Each probe consumes an active-torrent slot until the delete lands, so
        // callers must keep batches small.
        let futures: Vec<_> = hashes
            .iter()
            .map(|hash| {
                let hash = hash.to_string();
                let client = http_client.clone();
                let key = api_key.to_string();
                async move {
                    let magnet = format!("magnet:?xt=urn:btih:{hash}");

                    // Add the magnet
                    let add_url = format!("{BASE_URL}/torrents/addMagnet");
                    let add_resp = client
                        .post(&add_url)
                        .header("Authorization", auth_header(&key))
                        .form(&[("magnet", &magnet)])
                        .send()
                        .await;

                    let torrent_id = match add_resp {
                        Ok(resp) => {
                            let resp = match check_error_response(resp).await {
                                Ok(r) => r,
                                Err(_) => return None,
                            };
                            match resp.json::<RdAddMagnetResponse>().await {
                                Ok(r) => r.id,
                                Err(_) => return None,
                            }
                        }
                        Err(_) => return None,
                    };

                    // Check status
                    let info_url = format!("{BASE_URL}/torrents/info/{torrent_id}");
                    let is_cached = match client
                        .get(&info_url)
                        .header("Authorization", auth_header(&key))
                        .send()
                        .await
                    {
                        Ok(resp) => {
                            if let Ok(resp) = check_error_response(resp).await
                                && let Ok(info) = resp.json::<RdTorrentInfo>().await
                            {
                                matches!(
                                    info.status.as_str(),
                                    "waiting_files_selection" | "downloaded"
                                )
                            } else {
                                false
                            }
                        }
                        Err(_) => false,
                    };

                    // Always clean up
                    let delete_url = format!("{BASE_URL}/torrents/delete/{torrent_id}");
                    let _ = client
                        .delete(&delete_url)
                        .header("Authorization", auth_header(&key))
                        .send()
                        .await;

                    if is_cached {
                        Some(CachedAvailability {
                            hash,
                            available: true,
                            file_variants: vec![],
                        })
                    } else {
                        None
                    }
                }
            })
            .collect();

        let results: Vec<CachedAvailability> = futures::future::join_all(futures)
            .await
            .into_iter()
            .flatten()
            .collect();

        Ok(results)
    }

    async fn add_magnet(
        &self,
        http_client: &Client,
        api_key: &str,
        magnet: &str,
    ) -> Result<String, DebridError> {
        let url = format!("{BASE_URL}/torrents/addMagnet");
        let resp = http_client
            .post(&url)
            .header("Authorization", auth_header(api_key))
            .form(&[("magnet", magnet)])
            .send()
            .await?;

        let resp = check_error_response(resp).await?;
        let result: RdAddMagnetResponse = resp
            .json()
            .await
            .map_err(|e| DebridError::Parse(format!("Failed to parse addMagnet response: {e}")))?;

        Ok(result.id)
    }

    async fn select_files(
        &self,
        http_client: &Client,
        api_key: &str,
        torrent_id: &str,
        file_ids: &[i32],
    ) -> Result<(), DebridError> {
        let url = format!("{BASE_URL}/torrents/selectFiles/{torrent_id}");
        let files_str = if file_ids.is_empty() {
            "all".to_string()
        } else {
            file_ids
                .iter()
                .map(|id| id.to_string())
                .collect::<Vec<_>>()
                .join(",")
        };

        let resp = http_client
            .post(&url)
            .header("Authorization", auth_header(api_key))
            .form(&[("files", &files_str)])
            .send()
            .await?;

        // selectFiles returns 204 on success
        if resp.status() == reqwest::StatusCode::NO_CONTENT || resp.status().is_success() {
            return Ok(());
        }

        check_error_response(resp).await?;
        Ok(())
    }

    async fn get_torrent(
        &self,
        http_client: &Client,
        api_key: &str,
        torrent_id: &str,
    ) -> Result<DebridTorrent, DebridError> {
        let url = format!("{BASE_URL}/torrents/info/{torrent_id}");
        let resp = http_client
            .get(&url)
            .header("Authorization", auth_header(api_key))
            .send()
            .await?;

        let resp = check_error_response(resp).await?;
        let info: RdTorrentInfo = resp
            .json()
            .await
            .map_err(|e| DebridError::Parse(format!("Failed to parse torrent info: {e}")))?;

        let status = map_torrent_status(&info.status, info.progress);

        let files = info
            .files
            .into_iter()
            .map(|f| DebridFile {
                id: f.id,
                path: f.path,
                size: f.bytes,
                selected: f.selected == Some(1),
            })
            .collect();

        Ok(DebridTorrent {
            id: info.id,
            status,
            files,
            links: info.links,
        })
    }

    async fn unrestrict_link(
        &self,
        http_client: &Client,
        api_key: &str,
        link: &str,
    ) -> Result<UnrestrictedLink, DebridError> {
        let url = format!("{BASE_URL}/unrestrict/link");
        let resp = http_client
            .post(&url)
            .header("Authorization", auth_header(api_key))
            .form(&[("link", link)])
            .send()
            .await?;

        let resp = check_error_response(resp).await?;
        let result: RdUnrestrictResponse = resp
            .json()
            .await
            .map_err(|e| DebridError::Parse(format!("Failed to parse unrestrict response: {e}")))?;

        Ok(UnrestrictedLink {
            url: result.download,
            filename: result.filename,
            filesize: result.filesize,
            mime_type: result.mime_type,
            expires_at: None, // RD doesn't expose expiry in response
        })
    }

    async fn find_existing_torrents(
        &self,
        http_client: &Client,
        api_key: &str,
    ) -> Result<std::collections::HashMap<String, Vec<ExistingTorrent>>, DebridError> {
        // One page, newest-first, so a single call stays off the rate-limit budget.
        // Search also reads it to find copies of filter-gated releases added from
        // elsewhere (Stremio addons) — RD refuses re-adding those, so a copy missed
        // here is unplayable. 500 missed most of a 1576-torrent account. Docs cap
        // `limit` at 5000.
        let url = format!("{BASE_URL}/torrents?limit=2500");
        let resp = http_client
            .get(&url)
            .header("Authorization", auth_header(api_key))
            .send()
            .await?;

        let resp = check_error_response(resp).await?;
        let items: Vec<RdTorrentListItem> = resp
            .json()
            .await
            .map_err(|e| DebridError::Parse(format!("Failed to parse torrent list: {e}")))?;

        let mut out: std::collections::HashMap<String, Vec<ExistingTorrent>> =
            std::collections::HashMap::new();
        for item in items {
            let hash = item.hash.to_lowercase();
            let status = map_torrent_status(&item.status, None);
            let has_links = !item.links.is_empty();
            // Skip entries in a terminal failure state: 25 of the 200 newest on
            // a live account had no healthy copy at all, and reusing one would
            // pin that hash to a dead torrent permanently. Falling through to
            // add_magnet gives it a fresh attempt instead.
            if matches!(
                status,
                DebridTorrentStatus::Error { .. } | DebridTorrentStatus::Blocked
            ) {
                continue;
            }

            out.entry(hash).or_default().push(ExistingTorrent {
                id: item.id,
                status,
                has_links,
            });
        }
        for copies in out.values_mut() {
            copies.sort_by_key(|t| std::cmp::Reverse(t.usefulness()));
        }

        Ok(out)
    }

    async fn delete_torrent(
        &self,
        http_client: &Client,
        api_key: &str,
        torrent_id: &str,
    ) -> Result<(), DebridError> {
        let url = format!("{BASE_URL}/torrents/delete/{torrent_id}");
        let resp = http_client
            .delete(&url)
            .header("Authorization", auth_header(api_key))
            .send()
            .await?;

        // delete returns 204; treat an already-gone torrent as success.
        if resp.status().is_success() || resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Ok(());
        }

        check_error_response(resp).await?;
        Ok(())
    }

    async fn active_slots(
        &self,
        http_client: &Client,
        api_key: &str,
    ) -> Result<Option<ActiveSlots>, DebridError> {
        let url = format!("{BASE_URL}/torrents/activeCount");
        let resp = http_client
            .get(&url)
            .header("Authorization", auth_header(api_key))
            .send()
            .await?;

        let resp = check_error_response(resp).await?;
        let counts: RdActiveCountResponse = resp
            .json()
            .await
            .map_err(|e| DebridError::Parse(format!("Failed to parse activeCount: {e}")))?;

        Ok(Some(ActiveSlots {
            current: counts.nb,
            limit: counts.limit,
        }))
    }

    async fn get_account_status(
        &self,
        http_client: &Client,
        api_key: &str,
    ) -> Result<DebridAccountStatus, DebridError> {
        let url = format!("{BASE_URL}/user");
        let resp = http_client
            .get(&url)
            .header("Authorization", auth_header(api_key))
            .send()
            .await?;

        let resp = check_error_response(resp).await?;
        let user: RdUserResponse = resp
            .json()
            .await
            .map_err(|e| DebridError::Parse(format!("Failed to parse user response: {e}")))?;

        let expiration = time::OffsetDateTime::parse(
            &user.expiration,
            &time::format_description::well_known::Iso8601::DEFAULT,
        )
        .ok()
        .map(|dt| {
            dt.format(&time::format_description::well_known::Rfc3339)
                .unwrap_or_else(|_| user.expiration.clone())
        });

        let premium = user.account_type == "premium";

        Ok(DebridAccountStatus {
            provider: "Real-Debrid".to_string(),
            username: Some(user.username),
            expiration,
            premium,
            remaining_traffic: user.points.map(|p| format!("{p} points")),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_provider_type() {
        let provider = RealDebridProvider;
        assert_eq!(provider.provider_type(), DebridProviderType::RealDebrid);
        assert_eq!(provider.max_hashes_per_batch(), 20);
        assert!(!provider.supports_usenet());
    }

    #[tokio::test]
    async fn test_error_codes_win_over_http_status() {
        let cases = [
            (
                r#"{"error":"hoster_unavailable","error_code":19}"#,
                "HosterUnavailable",
            ),
            (r#"{"error":"infringing_file","error_code":35}"#, "Blocked"),
            (r#"{"error":"service_unavailable"}"#, "Status("),
            (
                r#"{"error":"traffic_exhausted","error_code":21}"#,
                "FairUsage",
            ),
            (
                r#"{"error":"too_many_active_downloads","error_code":23}"#,
                "FairUsage",
            ),
            (r#"{"error":"torrent_too_big","error_code":29}"#, "TooBig"),
            (
                r#"{"error":"permission_denied","error_code":9}"#,
                "AuthFailed",
            ),
            (
                r#"{"error":"hoster_not_free","error_code":20}"#,
                "AuthFailed",
            ),
        ];
        for (body, expected) in cases {
            let resp = http::Response::builder()
                .status(503)
                .body(body.to_string())
                .unwrap();
            let err = check_error_response(reqwest::Response::from(resp))
                .await
                .unwrap_err();
            assert!(
                format!("{err:?}").starts_with(expected),
                "{body} -> {err:?}"
            );
        }
    }

    #[test]
    fn test_rd_torrent_status_mapping() {
        use DebridTorrentStatus as S;
        type Expect = fn(&S) -> bool;
        let cases: [(&str, Expect); 6] = [
            ("downloaded", |s| matches!(s, S::Ready)),
            // RD's status for a finished torrent whose swarm died: its files still stream.
            ("dead", |s| matches!(s, S::Ready)),
            ("downloading", |s| matches!(s, S::Downloading { .. })),
            ("queued", |s| matches!(s, S::Downloading { .. })),
            ("magnet_error", |s| matches!(s, S::Error { .. })),
            ("error", |s| matches!(s, S::Error { .. })),
        ];
        for (status, expected) in cases {
            let got = map_torrent_status(status, Some(100.0));
            assert!(expected(&got), "{status} -> {got:?}");
        }
    }
}
