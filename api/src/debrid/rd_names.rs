use std::sync::LazyLock;

use regex::Regex;

// Debrid Media Manager's measurement of 2026-10-14 (src/utils/deInfringe.ts there). RD changed
// this three times in two weeks, so a name that matches is a hint, never a verdict.
static EXACT_CASE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"WEB-DL|WEB\.(?:x264|H264)").expect("valid regex"));
static ANY_CASE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:web|bd|hd|dvd)rip|bluray\.x264|p\.bluray\.dts|hdtv\.(?:x264|xvid)")
        .expect("valid regex")
});

/// Real-Debrid refuses every add of a torrent whose name carries one of these tags, while
/// its refusal of anything else is usually an account-wide pause on adds.
pub fn refuses(name: &str) -> bool {
    EXACT_CASE.is_match(name) || ANY_CASE.is_match(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_measured_tags_are_refused() {
        let cases = [
            ("Movie.2020.1080p.WEB-DL.DDP5.1-GRP", true),
            ("Movie.2020.1080p.web-dl-GRP", false),
            ("Movie.2020.1080p.WEB.DL-GRP", false),
            ("Movie.2020.1080p.WEB.H264-GRP", true),
            ("Movie.2020.1080p.WEB.h264-GRP", false),
            ("Movie.2020.720p.WEBRip.x264", true),
            ("Movie.2020.bdrip", true),
            ("Movie.2020.BD-Rip", false),
            ("Show.S01E01.HDTV.XviD-GRP", true),
            ("Show.S01E01.hdtv.H264-GRP", false),
            ("Movie.2020.1080p.BluRay.x264-GRP", true),
            ("Movie.2020.1080p.BluRay-x264-GRP", false),
            ("Movie.2020.1080p.BluRay.DTS.x264", true),
            ("Movie.2020.REMUX.BluRay.DTS-HD", false),
            ("Movie.2020.2160p.WEB-DLRip", true),
            ("Movie.2020.2160p.BluRay.REMUX.HEVC", false),
            ("Щенячий патруль / Paw Patrol S01 WEB.DL 1080p", false),
        ];
        for (name, want) in cases {
            assert_eq!(refuses(name), want, "{name}");
        }
    }
}
