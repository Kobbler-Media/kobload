use reqwest::StatusCode;

use super::usenet_failure::indexer_refused;
use crate::payload_fetch::{PayloadClients, PayloadError, PayloadGet};
use crate::redirect_guard::Origin;

/// Season-pack NZBs run to tens of MB; anything past this is not an NZB worth importing.
pub const MAX_NZB_BYTES: usize = 128 << 20;
const NZB_ROOT_WINDOW: usize = 4096;

#[derive(Debug, Clone)]
pub enum FetchError {
    Unreachable(String),
    Refused(StatusCode),
    TooLarge,
}

impl FetchError {
    pub fn message(&self) -> String {
        match self {
            Self::Unreachable(e) => format!("Could not fetch the NZB from the indexer: {e}"),
            Self::Refused(status) => indexer_refused(*status),
            Self::TooLarge => format!(
                "Usenet release is not cached: its NZB is over {} MB",
                MAX_NZB_BYTES >> 20
            ),
        }
    }

    /// An answer from the indexer stays the answer for a while; a network error does not.
    pub fn remembered(&self) -> bool {
        !matches!(self, Self::Unreachable(_))
    }

    /// Whether the answer is about this release. Auth failures, rate limits and outages
    /// would otherwise demote every release the indexer is asked for.
    pub fn condemns_release(&self) -> bool {
        match self {
            Self::Refused(status) => matches!(status.as_u16(), 404 | 410),
            Self::TooLarge => true,
            Self::Unreachable(_) => false,
        }
    }
}

// Indexers report API limits as an XML error with a 200, which the engine can only call an
// invalid NZB.
pub fn is_nzb_document(body: &[u8]) -> bool {
    body[..body.len().min(NZB_ROOT_WINDOW)]
        .to_ascii_lowercase()
        .windows(4)
        .any(|w| w == b"<nzb")
}

/// The URL carries the indexer's API key, so neither it nor reqwest's error (which quotes
/// it) is ever reported.
pub async fn fetch(download_url: &str, origin: Origin) -> Result<Vec<u8>, FetchError> {
    let unreachable = |e: reqwest::Error| FetchError::Unreachable(e.without_url().to_string());
    let mut resp = PayloadClients::shared()
        .get(PayloadGet {
            url: download_url,
            origin,
            ..Default::default()
        })
        .await
        .map_err(|e| match e {
            PayloadError::Request(e) => unreachable(e),
            refused @ PayloadError::Refused(_) => FetchError::Unreachable(refused.to_string()),
        })?;
    let status = resp.status();
    if !status.is_success() {
        return Err(FetchError::Refused(status));
    }
    if resp
        .content_length()
        .is_some_and(|len| len > MAX_NZB_BYTES as u64)
    {
        return Err(FetchError::TooLarge);
    }
    let mut body = Vec::new();
    while let Some(chunk) = resp.chunk().await.map_err(unreachable)? {
        if body.len() + chunk.len() > MAX_NZB_BYTES {
            return Err(FetchError::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}
