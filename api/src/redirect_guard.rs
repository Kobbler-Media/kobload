use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use reqwest::Url;
use reqwest::redirect::{Attempt, Policy};

const MAX_REDIRECTS: usize = 10;

// Release URLs come from indexers, so a public indexer must not be able to bounce a fetch
// onto the LAN. A LAN indexer still may: the operator pointed kobload there.
pub fn policy() -> Policy {
    Policy::custom(|attempt: Attempt<'_>| {
        if attempt.previous().len() > MAX_REDIRECTS {
            return attempt.error("too many redirects");
        }
        let allowed = attempt
            .previous()
            .first()
            .is_none_or(|original| redirect_allowed(original, attempt.url()));
        if allowed {
            attempt.follow()
        } else {
            attempt.error("refused a redirect from a public host to a private address")
        }
    })
}

pub fn redirect_allowed(original: &Url, next: &Url) -> bool {
    internal(original) || !internal(next)
}

// Hostnames are judged by their shape, not resolved: single-label names are container or
// LAN names, and the reserved suffixes never reach the internet.
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
    ip.is_private() || ip.is_loopback() || ip.is_link_local() || ip.is_unspecified()
}

fn internal_v6(ip: Ipv6Addr) -> bool {
    ip.is_loopback() || ip.is_unspecified() || ip.is_unique_local() || ip.is_unicast_link_local()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_public_host_cannot_redirect_onto_the_lan_but_a_lan_host_can() {
        let cases = [
            (
                "https://indexer.example/nzb",
                "https://cdn.example/x.nzb",
                true,
            ),
            (
                "https://indexer.example/nzb",
                "http://127.0.0.1:5432/",
                false,
            ),
            ("https://indexer.example/nzb", "http://10.0.0.208/", false),
            ("https://indexer.example/nzb", "http://192.168.1.1/", false),
            ("https://indexer.example/nzb", "http://172.16.0.1/", false),
            (
                "https://indexer.example/nzb",
                "http://169.254.169.254/latest",
                false,
            ),
            ("https://indexer.example/nzb", "http://[::1]/", false),
            ("https://indexer.example/nzb", "http://[fe80::1]/", false),
            ("https://indexer.example/nzb", "http://[fd00::1]/", false),
            (
                "https://indexer.example/nzb",
                "http://[::ffff:10.0.0.1]/",
                false,
            ),
            ("https://indexer.example/nzb", "http://0.0.0.0/", false),
            (
                "https://indexer.example/nzb",
                "http://localhost:8080/",
                false,
            ),
            (
                "https://indexer.example/nzb",
                "http://postgres:5432/",
                false,
            ),
            ("https://indexer.example/nzb", "http://nas.lan/", false),
            (
                "http://192.168.1.5:9696/nzb",
                "http://192.168.1.6/x.nzb",
                true,
            ),
            ("http://prowlarr:9696/nzb", "http://sab:8080/x.nzb", true),
            (
                "http://127.0.0.1:9696/nzb",
                "https://indexer.example/x",
                true,
            ),
        ];
        for (original, next, allowed) in cases {
            assert_eq!(
                redirect_allowed(&original.parse().unwrap(), &next.parse().unwrap()),
                allowed,
                "{original} -> {next}"
            );
        }
    }
}
