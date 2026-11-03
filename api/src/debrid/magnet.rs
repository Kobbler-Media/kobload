//! Long-lived public trackers, so RD can find peers for a release it has not cached yet
//! without relying on DHT alone. Russian trackers only announce Russian releases.

const TRACKERS: &[&str] = &[
    "udp://tracker.opentrackr.org:1337/announce",
    "udp://open.demonii.com:1337/announce",
    "udp://open.stealth.si:80/announce",
    "udp://tracker.torrent.eu.org:451/announce",
    "udp://exodus.desync.com:6969/announce",
    "udp://explodie.org:6969/announce",
];

const RUSSIAN_TRACKERS: &[&str] = &[
    "udp://opentor.net:6969",
    "http://bt.t-ru.org/ann?magnet",
    "http://bt2.t-ru.org/ann?magnet",
    "http://bt3.t-ru.org/ann?magnet",
    "http://bt4.t-ru.org/ann?magnet",
];

/// The magnet as it is sent to the provider: named, so the throttle can tell a refusal of
/// the name from a pause, and with trackers.
pub fn for_add(magnet: &str, release_title: Option<&str>) -> String {
    let mut out = magnet.to_string();
    let title = release_title
        .and_then(|t| t.lines().next())
        .map(str::trim)
        .filter(|t| !t.is_empty());
    if let Some(title) = title
        && display_name(magnet).is_none()
    {
        out.push_str("&dn=");
        out.push_str(&encode(title));
    }
    if magnet.contains("&tr=") {
        return out;
    }
    let russian =
        release_title.is_some_and(|t| t.chars().any(|c| matches!(c, '\u{0400}'..='\u{04FF}')));
    let extra = if russian { RUSSIAN_TRACKERS } else { &[] };
    for tracker in TRACKERS.iter().chain(extra) {
        out.push_str("&tr=");
        out.push_str(&encode(tracker));
    }
    out
}

pub fn display_name(magnet: &str) -> Option<String> {
    reqwest::Url::parse(magnet)
        .ok()?
        .query_pairs()
        .find(|(k, _)| k == "dn")
        .map(|(_, v)| v.into_owned())
        .filter(|v| !v.is_empty())
}

fn encode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                (b as char).to_string()
            }
            b => format!("%{b:02X}"),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_title_names_an_unnamed_magnet_and_reads_back() {
        let hash = "magnet:?xt=urn:btih:abc";
        let title = "Щенячий патруль / PAW Patrol S01 1080p WEB-DL\n👤 12";
        let sent = for_add(hash, Some(title));
        assert_eq!(
            display_name(&sent).as_deref(),
            Some("Щенячий патруль / PAW Patrol S01 1080p WEB-DL")
        );
        assert!(sent.contains("t-ru.org"));
        let named = "magnet:?xt=urn:btih:abc&dn=Own+Name";
        assert_eq!(
            display_name(&for_add(named, Some(title))).as_deref(),
            Some("Own Name")
        );
        assert_eq!(display_name(&for_add(hash, None)), None);
    }
}
