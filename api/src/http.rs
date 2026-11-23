//! Shared HTTP client configuration.
//!
//! All outbound HTTP clients should use `new_client()` to get a clone of the
//! shared application-wide client. This ensures consistent headers and
//! connection pooling across the application.
//!
//! `reqwest::Client` uses `Arc` internally, so cloning is cheap — all clones
//! share the same connection pool.

use reqwest::{Client, ClientBuilder};
use std::sync::LazyLock;

/// Shared application-wide HTTP client.
///
/// Single connection pool reused by all callers of `new_client()`.
static SHARED_CLIENT: LazyLock<Client> = LazyLock::new(|| {
    builder()
        .user_agent("Kobload/1.0")
        .build()
        .expect("Failed to create HTTP client")
});

/// Default headers applied to all outbound HTTP clients.
fn default_headers() -> reqwest::header::HeaderMap {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::ACCEPT_LANGUAGE,
        "en-US,en;q=0.9".parse().unwrap(),
    );
    headers
}

/// Get a clone of the shared HTTP client.
///
/// All clones share the same connection pool, so this is both cheap and
/// efficient for connection reuse. Sets `Accept-Language: en-US,en;q=0.9`
/// and a 30s timeout.
pub fn new_client() -> Client {
    SHARED_CLIENT.clone()
}

/// Get a pre-configured `ClientBuilder` with default headers and timeout.
///
/// Use this only when you need a separate client with different settings
/// (e.g., longer timeout for file downloads). The resulting client will have
/// its own connection pool.
pub fn builder() -> ClientBuilder {
    Client::builder()
        .default_headers(default_headers())
        .gzip(true)
        .brotli(true)
        .deflate(true)
        .connect_timeout(std::time::Duration::from_secs(10))
        .timeout(std::time::Duration::from_secs(30))
        .tcp_keepalive(std::time::Duration::from_secs(60))
        .pool_max_idle_per_host(10)
}
