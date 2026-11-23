use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use reqwest::Url;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Origin {
    // A URL an operator or API caller supplied directly.
    #[default]
    Operator,
    // `None` when the indexer has no configured URL, so it is judged as public.
    Indexer(Option<Url>),
}

impl Origin {
    pub fn indexer(base_url: Option<&str>) -> Self {
        Self::Indexer(base_url.and_then(|url| url.trim().parse().ok()))
    }

    fn configured(&self) -> Option<&Url> {
        match self {
            Self::Indexer(url) => url.as_ref(),
            Self::Operator => None,
        }
    }

    // A name may be public in shape yet resolve onto the LAN; the operator configured it,
    // so that makes the indexer a LAN indexer.
    pub async fn trust(&self) -> Trust<'_> {
        let lan = match self.configured() {
            Some(url) if internal(url) => true,
            Some(url) => resolves_internal(url).await,
            None => false,
        };
        Trust { origin: self, lan }
    }
}

async fn resolves_internal(url: &Url) -> bool {
    let (Some(host), Some(port)) = (url.host_str(), url.port_or_known_default()) else {
        return false;
    };
    let lookup = tokio::net::lookup_host((host, port));
    match tokio::time::timeout(std::time::Duration::from_secs(2), lookup).await {
        Ok(Ok(mut addrs)) => addrs.any(|a| internal_ip(a.ip())),
        _ => false,
    }
}

#[derive(Debug, Clone, Copy)]
pub struct Trust<'a> {
    origin: &'a Origin,
    lan: bool,
}

impl Trust<'_> {
    #[cfg(test)]
    fn new(origin: &Origin, lan: bool) -> Trust<'_> {
        Trust { origin, lan }
    }

    fn on_lan(&self, url: &Url) -> bool {
        internal(url) || (self.lan && self.is_origin(url))
    }

    fn is_origin(&self, url: &Url) -> bool {
        self.origin.configured().is_some_and(|o| same_host(o, url))
    }

    pub fn entry_allowed(&self, url: &Url) -> bool {
        match self.origin {
            Origin::Operator => true,
            Origin::Indexer(_) => self.lan || self.is_origin(url) || !internal(url),
        }
    }

    pub fn hop_allowed(&self, previous: &Url, next: &Url) -> bool {
        self.on_lan(previous) || !internal(next)
    }

    // Names reached from a public host go through the resolver that refuses private answers.
    pub fn may_resolve_privately(&self, previous: Option<&Url>, url: &Url) -> bool {
        match previous {
            Some(previous) => self.on_lan(previous),
            None => match self.origin {
                Origin::Operator => true,
                Origin::Indexer(_) => self.lan || self.is_origin(url),
            },
        }
    }
}

pub fn same_host(a: &Url, b: &Url) -> bool {
    a.host_str()
        .zip(b.host_str())
        .is_some_and(|(a, b)| a.eq_ignore_ascii_case(b))
        && a.port_or_known_default() == b.port_or_known_default()
}

pub fn refuse_internal(addrs: Vec<SocketAddr>) -> Result<Vec<SocketAddr>, &'static str> {
    if addrs.is_empty() {
        return Err("the name did not resolve");
    }
    if addrs.iter().any(|a| internal_ip(a.ip())) {
        return Err("a public name resolved to a private address");
    }
    Ok(addrs)
}

// Hostnames are judged by shape here: single-label names are container or LAN names, and
// the reserved suffixes never reach the internet.
fn internal(url: &Url) -> bool {
    let Some(host) = url.host_str() else {
        return true;
    };
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    if let Ok(ip) = bare.parse::<IpAddr>() {
        return internal_ip(ip);
    }
    let name = host.trim_end_matches('.').to_ascii_lowercase();
    !name.contains('.')
        || [".localhost", ".local", ".lan", ".internal", ".home.arpa"]
            .iter()
            .any(|suffix| name.ends_with(suffix))
}

fn internal_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(ip) => internal_v4(ip),
        IpAddr::V6(ip) => match ip.to_ipv4_mapped() {
            Some(v4) => internal_v4(v4),
            None => internal_v6(ip),
        },
    }
}

fn internal_v4(ip: Ipv4Addr) -> bool {
    let [a, b, ..] = ip.octets();
    let cgnat = a == 100 && (b & 0xC0) == 64;
    cgnat
        || ip.is_private()
        || ip.is_loopback()
        || ip.is_link_local()
        || ip.is_unspecified()
        || ip.is_broadcast()
}

fn internal_v6(ip: Ipv6Addr) -> bool {
    ip.is_loopback() || ip.is_unspecified() || ip.is_unique_local() || ip.is_unicast_link_local()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn url(raw: &str) -> Url {
        raw.parse().unwrap()
    }

    #[test]
    fn each_hop_is_judged_against_the_one_before_it() {
        let origin = Origin::indexer(Some("http://192.168.1.5:9696"));
        let trust = Trust::new(&origin, true);
        for (previous, next, allowed) in [
            ("http://192.168.1.5:9696/n", "http://192.168.1.6/x", true),
            ("http://192.168.1.5:9696/n", "https://cdn.example/x", true),
            ("https://cdn.example/x", "http://192.168.1.6/x", false),
            ("https://cdn.example/x", "http://127.0.0.1:5432/", false),
            ("https://cdn.example/x", "http://169.254.169.254/", false),
            ("https://cdn.example/x", "http://100.64.0.1/", false),
            ("https://cdn.example/x", "http://[::1]/", false),
            ("https://cdn.example/x", "http://[fe80::1]/", false),
            ("https://cdn.example/x", "http://[fd00::1]/", false),
            ("https://cdn.example/x", "http://[::ffff:10.0.0.1]/", false),
            ("https://cdn.example/x", "http://postgres:5432/", false),
            ("https://cdn.example/x", "http://nas.lan/", false),
            ("https://cdn.example/x", "https://other.example/x", true),
        ] {
            assert_eq!(
                trust.hop_allowed(&url(previous), &url(next)),
                allowed,
                "{previous} -> {next}"
            );
        }
    }

    #[test]
    fn a_public_indexer_cannot_hand_over_a_lan_url() {
        let public = Origin::indexer(Some("https://indexer.example"));
        let unknown = Origin::Indexer(None);
        let lan = Origin::indexer(Some("http://prowlarr:9696"));
        for (origin, is_lan, target, allowed) in [
            (&public, false, "https://indexer.example/d", true),
            (&public, false, "https://cdn.example/d", true),
            (&public, false, "http://10.0.0.208/d", false),
            (&unknown, false, "http://127.0.0.1/d", false),
            (&unknown, false, "https://cdn.example/d", true),
            (&lan, true, "http://prowlarr:9696/d", true),
            (&lan, true, "http://10.0.0.5/d", true),
            (&Origin::Operator, false, "http://10.0.0.5/d", true),
        ] {
            assert_eq!(
                Trust::new(origin, is_lan).entry_allowed(&url(target)),
                allowed,
                "{origin:?} {target}"
            );
        }
    }

    #[test]
    fn a_name_resolving_to_a_private_address_is_refused() {
        let at = |ip: &str| SocketAddr::new(ip.parse().unwrap(), 443);
        for bad in [
            "10.0.0.1",
            "127.0.0.1",
            "169.254.1.1",
            "100.100.0.1",
            "::1",
            "fd00::1",
        ] {
            assert!(
                refuse_internal(vec![at("93.184.216.34"), at(bad)]).is_err(),
                "{bad}"
            );
        }
        assert!(refuse_internal(Vec::new()).is_err());
        assert!(refuse_internal(vec![at("93.184.216.34")]).is_ok());
    }
}
