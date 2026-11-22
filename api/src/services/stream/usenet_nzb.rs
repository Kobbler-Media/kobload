use reqwest::StatusCode;

use super::usenet_failure::indexer_refused;

/// Season-pack NZBs run to tens of MB; anything past this is not an NZB worth importing.
pub const MAX_NZB_BYTES: usize = 128 << 20;

#[derive(Debug)]
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
}

/// The URL carries the indexer's API key, so neither it nor reqwest's error (which quotes
/// it) is ever reported.
pub async fn fetch(
    http_client: &reqwest::Client,
    download_url: &str,
) -> Result<Vec<u8>, FetchError> {
    let unreachable = |e: reqwest::Error| FetchError::Unreachable(e.without_url().to_string());
    let mut resp = http_client
        .get(download_url)
        .send()
        .await
        .map_err(unreachable)?;
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
