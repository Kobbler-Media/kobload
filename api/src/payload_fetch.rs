use std::net::SocketAddr;
use std::sync::{Arc, LazyLock};

use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use reqwest::header::{COOKIE, LOCATION};
use reqwest::redirect::Policy;
use reqwest::{Client, ClientBuilder, Response, Url};

use crate::redirect_guard::{Origin, refuse_internal, same_host};

const MAX_REDIRECTS: usize = 10;

#[derive(Debug, thiserror::Error)]
pub enum PayloadError {
    #[error(transparent)]
    Request(#[from] reqwest::Error),
    #[error("refused to fetch the release: {0}")]
    Refused(&'static str),
}

#[derive(Default)]
pub struct PayloadGet<'a> {
    pub url: &'a str,
    pub origin: Origin,
    pub auth_header: Option<&'a (String, String)>,
    pub cookie: Option<&'a str>,
}

pub struct SystemDns;

impl Resolve for SystemDns {
    fn resolve(&self, name: Name) -> Resolving {
        Box::pin(async move {
            let addrs: Vec<SocketAddr> =
                tokio::net::lookup_host((name.as_str(), 0)).await?.collect();
            Ok(Box::new(addrs.into_iter()) as Addrs)
        })
    }
}

struct PublicOnly<R>(Arc<R>);

impl<R: Resolve + 'static> Resolve for PublicOnly<R> {
    fn resolve(&self, name: Name) -> Resolving {
        let dns = Arc::clone(&self.0);
        Box::pin(async move {
            let addrs: Vec<SocketAddr> = dns.resolve(name).await?.collect();
            let addrs = refuse_internal(addrs)?;
            Ok(Box::new(addrs.into_iter()) as Addrs)
        })
    }
}

// Redirects are followed by hand: reqwest's policy cannot drop a custom auth header or
// switch resolver between hops.
pub struct PayloadClients {
    plain: Client,
    public_only: Client,
}

static SHARED: LazyLock<PayloadClients> = LazyLock::new(|| PayloadClients::build(|b| b));

impl PayloadClients {
    pub fn shared() -> &'static Self {
        &SHARED
    }

    pub fn build(customize: impl Fn(ClientBuilder) -> ClientBuilder) -> Self {
        Self::with_dns(Arc::new(SystemDns), customize)
    }

    pub fn with_dns<R: Resolve + 'static>(
        dns: Arc<R>,
        customize: impl Fn(ClientBuilder) -> ClientBuilder,
    ) -> Self {
        let base = || {
            customize(
                crate::http::builder()
                    .user_agent("Kobload/1.0")
                    .redirect(Policy::none()),
            )
        };
        Self {
            plain: base()
                .dns_resolver(Arc::clone(&dns))
                .build()
                .expect("Failed to create HTTP client"),
            // A proxy would resolve the name itself, past the check.
            public_only: base()
                .no_proxy()
                .dns_resolver(Arc::new(PublicOnly(dns)))
                .build()
                .expect("Failed to create HTTP client"),
        }
    }

    pub async fn get(&self, request: PayloadGet<'_>) -> Result<Response, PayloadError> {
        let trust = request.origin.trust();
        let mut url: Url = request
            .url
            .parse()
            .map_err(|_| PayloadError::Refused("the URL is not valid"))?;
        if !trust.entry_allowed(&url) {
            return Err(PayloadError::Refused(
                "a public indexer handed out a private address",
            ));
        }
        let first = url.clone();
        let mut credentials = true;
        let mut previous: Option<Url> = None;
        for _ in 0..=MAX_REDIRECTS {
            credentials = credentials && same_host(&first, &url);
            let client = if trust.may_resolve_privately(previous.as_ref(), &url) {
                &self.plain
            } else {
                &self.public_only
            };
            let mut builder = client.get(url.clone());
            if credentials {
                if let Some((name, value)) = request.auth_header {
                    builder = builder.header(name, value);
                }
                if let Some(cookie) = request.cookie {
                    builder = builder.header(COOKIE, cookie);
                }
            }
            let response = builder.send().await?;
            if !response.status().is_redirection() {
                return Ok(response);
            }
            let Some(next) = response
                .headers()
                .get(LOCATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|location| url.join(location).ok())
                .filter(|next| matches!(next.scheme(), "http" | "https"))
            else {
                return Ok(response);
            };
            if !trust.hop_allowed(&url, &next) {
                return Err(PayloadError::Refused(
                    "a public host redirected to a private address",
                ));
            }
            previous = Some(std::mem::replace(&mut url, next));
        }
        Err(PayloadError::Refused("too many redirects"))
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr as _;

    use super::*;

    #[tokio::test]
    async fn the_public_resolver_refuses_a_name_on_loopback() {
        let name = Name::from_str("localhost").unwrap();
        assert!(PublicOnly(Arc::new(SystemDns)).resolve(name).await.is_err());
    }
}
