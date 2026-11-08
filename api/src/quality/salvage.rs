//! Source tags the word matchers in `detect_source` can't see: glued to a
//! suffix or a resolution (`HDTVRip`, `WEBDLRip`, `BDMux`, `BDRip720p`,
//! `PreDVDRip`), set off by `/` or `,` (`[2016, HDTV, x264]`), or misspelt
//! (`telesyndl`). Rather than listing every spelling, each tag token has its
//! resolution and up to two suffixes peeled off and the stem is looked up; the
//! long pre-release stems also match with one typo.
//!
//! Only the tag region is read, so `Webster.2019.1080p.BluRay` can't trip it.
//! Fuzzy matches only count after the resolution, where episode titles
//! (`S01E02.The.Screened.Room.1080p`) can't reach.
//!
//! It runs on every parse (a pre-release tag has to be able to overrule a WEB
//! one), so it allocates nothing: one regex find, then a split of the tag
//! region and a few `ends_with`/`match` per token.

use super::{TAG_REGION_RE, source_tier};

const SUFFIXES: &[&str] = &["rip", "mux", "dl", "hd"];

/// Pre-release stems long enough that a one-edit typo is still unambiguous.
/// Not `bluray`: `blurry` is one edit away, and a missed BluRay costs little.
const FUZZY_STEMS: &[&str] = &["telesync", "telecine", "screener", "workprint"];

/// The source this title's tag region names in a form `detect_source` misses.
/// A pre-release source wins over any other found.
pub(super) fn salvage_source(lower: &str) -> Option<&'static str> {
    let start = TAG_REGION_RE.find(lower)?.start();
    let tokens = || {
        lower[start..]
            .split(|c: char| !c.is_ascii_alphanumeric())
            .filter(|t| !t.is_empty())
    };
    let resolution_at = tokens().position(is_resolution);
    let mut found = None;
    for (i, token) in tokens().enumerate() {
        let Some((source, fuzzy)) = classify(token) else {
            continue;
        };
        if fuzzy && resolution_at.is_none_or(|r| i < r) {
            continue;
        }
        if source_tier(source) == Some(0) {
            return Some(source);
        }
        found = found.or(Some(source));
    }
    found
}

/// Combine `detect_source`'s answer with the salvaged one: fill a gap, and let
/// a pre-release tag win over a WEB/TV/DVD one the way `detect_source` does.
pub(super) fn refine_source(detected: Option<String>, lower: &str) -> Option<String> {
    if detected.as_deref().and_then(source_tier) == Some(0) {
        return detected;
    }
    match salvage_source(lower) {
        Some(s) if detected.is_none() || source_tier(s) == Some(0) => Some(s.to_string()),
        _ => detected,
    }
}

/// `1080p`, `720p50`, `1080i`, `4k`.
fn is_resolution(token: &str) -> bool {
    token == "4k" || resolution_len(token) == Some(token.len())
}

/// Length of a `\d{3,4}[pi]\d{0,3}` that ends `token`, if one does.
fn resolution_len(token: &str) -> Option<usize> {
    let b = token.as_bytes();
    let mut i = b.len();
    let fps = b.iter().rev().take_while(|c| c.is_ascii_digit()).count();
    if fps > 3 {
        return None;
    }
    i -= fps;
    if i == 0 || !matches!(b[i - 1], b'p' | b'i') {
        return None;
    }
    i -= 1;
    let digits = b[..i].iter().rev().take_while(|c| c.is_ascii_digit()).count();
    if !(3..=4).contains(&digits) {
        return None;
    }
    Some(b.len() - (i - digits))
}

/// The source a single token names, and whether it took a fuzzy match.
fn classify(token: &str) -> Option<(&'static str, bool)> {
    let mut stem = token;
    if let Some(n) = resolution_len(stem)
        && n < stem.len()
        && stem.as_bytes()[stem.len() - n - 1].is_ascii_alphabetic()
    {
        stem = &stem[..stem.len() - n];
    }
    // Peeled outermost first: `webdlrip` gives ["rip", "dl"].
    let mut peeled: [&str; 2] = [""; 2];
    let mut n = 0;
    while n < peeled.len()
        && let Some(&s) = SUFFIXES
            .iter()
            .find(|s| stem.len() >= s.len() + 2 && stem.ends_with(**s))
    {
        stem = &stem[..stem.len() - s.len()];
        peeled[n] = s;
        n += 1;
    }
    if let Some(source) = stem_source(stem, &peeled[..n]) {
        return Some((source, false));
    }
    if stem.len() >= 6 {
        return FUZZY_STEMS
            .iter()
            .find(|s| within_one_edit(stem.as_bytes(), s.as_bytes()))
            .map(|&s| (s, true));
    }
    None
}

fn stem_source(stem: &str, peeled: &[&str]) -> Option<&'static str> {
    let suffixed = !peeled.is_empty();
    Some(match stem {
        "telesync" | "hdts" => "telesync",
        "telecine" => "telecine",
        // PreDVD is the Indian scene's name for a theatre recording.
        "cam" | "hdcam" | "predvd" | "pdvd" => "cam",
        "screener" => "screener",
        // Bare `scr` is too short to overrule a WEB tag (or a `.scr` file).
        "scr" if suffixed => "screener",
        "dvdscr" => "dvdscr",
        "bdscr" => "bdscr",
        "workprint" => "workprint",
        "web" => match peeled.first() {
            None | Some(&"rip") | Some(&"mux") => "webrip",
            _ => "web-dl",
        },
        "bluray" | "blueray" => "bluray",
        "bd" | "br" if suffixed => "bluray",
        "hdtv" | "ahdtv" | "uhdtv" => "hdtv",
        "pdtv" => "pdtv",
        "sdtv" => "sdtv",
        "tv" if suffixed => "sdtv",
        "sat" if suffixed => "satellite",
        "dvd" if peeled.contains(&"rip") => "dvdrip",
        "dvd" => "dvd",
        "hddvd" => "hddvd",
        _ => return None,
    })
}

/// At most one insertion, deletion, substitution or adjacent transposition.
fn within_one_edit(a: &[u8], b: &[u8]) -> bool {
    let (long, short) = if a.len() >= b.len() { (a, b) } else { (b, a) };
    if long.len() - short.len() > 1 {
        return false;
    }
    let prefix = long.iter().zip(short).take_while(|(x, y)| x == y).count();
    if prefix == short.len() {
        return true;
    }
    if long.len() != short.len() {
        return long[prefix + 1..] == short[prefix..];
    }
    long[prefix + 1..] == short[prefix + 1..]
        || (prefix + 1 < long.len()
            && long[prefix] == short[prefix + 1]
            && long[prefix + 1] == short[prefix]
            && long[prefix + 2..] == short[prefix + 2..])
}

#[cfg(test)]
mod tests {
    use super::super::parse_quality;
    use super::*;

    fn source(title: &str) -> Option<String> {
        parse_quality(title).source
    }

    #[test]
    fn misspelt_or_glued_pre_release_is_caught() {
        for (title, want) in [
            ("clayface.2026.1080p.telesyndl.x264-cleanup", "telesync"),
            ("Movie.2026.1080p.TeleSyncDL.x264-GRP", "telesync"),
            ("Movie.2026.1080p.TeelSync.x264-GRP", "telesync"),
            ("Captain Miller (2024) Hindi 720p - HQ HDTSRip - x264 - AAC", "telesync"),
            ("Posledniy.bogatir.2026.O.TELECINERip.avi", "telecine"),
            ("Kurup (2021) [Malayalam - HQ-PreDVDRip - x264 - MP3]", "cam"),
            ("Alone (2015) Hindi 1CD DvdScrRip x264 Team DDH", "dvdscr"),
            ("The Ghazi Attack (2017) - 1CD -Desi ScrRip - x264 - AAC", "screener"),
            // ...and overrules the WEB tag next to it.
            ("Movie.2026.1080p.WEB-DL.telesyndl-GRP", "telesync"),
            ("Movie.2026.1080p.WEB-DL.PreDVDRip-GRP", "cam"),
        ] {
            assert_eq!(source(title).as_deref(), Some(want), "{title}");
        }
    }

    #[test]
    fn bare_scr_does_not_overrule_web() {
        let title = "Movie.2026.1080p.WEB-DL.x264.scr";
        assert_eq!(source(title).as_deref(), Some("web-dl"));
    }

    #[test]
    fn glued_and_oddly_separated_tags_fill_the_gap() {
        for (title, want) in [
            ("Motel Hell.1980.HDTVRip_by_Bathory.avi", "hdtv"),
            ("WWE.RAW.2015.12.14.720p.AHDTV.x264-WaLMaRT", "hdtv"),
            ("FBI.s01e13.WEBDLRip.NewStudio.TV.avi", "webrip"),
            ("The.Rook.S01E01.Chapter.1.1080p.HEVC.WEBMux.AC3.ITA-G66", "webrip"),
            ("The.IT.Crowd.S04.Ger.Eng.DL.720p.WebHD.x264-iND", "web-dl"),
            ("Peacemaker.S01E01-08.ITA.BDMux.x264-UBi", "bluray"),
            ("Jumanji.2017.BDRip1080p.60FPS.mkv", "bluray"),
            ("The Getaway (1994).HDDVDRip.1080p.10bit.HEVC", "hddvd"),
            ("Volleyball 2014 / Final [Sport, SATRip, XviD, RU]", "satellite"),
            ("WWE RAW 21.11.2016 [Wrestling, HDTV/1080i, x264]", "hdtv"),
            ("Philadelphia Experiment 2 [1993] (DVDRip, DivX, Rus).avi", "dvdrip"),
        ] {
            assert_eq!(source(title).as_deref(), Some(want), "{title}");
        }
    }

    #[test]
    fn title_words_and_unknown_tags_stay_unsourced() {
        for title in [
            "Webster.2019.1080p.x264-GRP",
            // An episode title before the resolution is not a tag.
            "Show.S01E02.The.Screened.Room.1080p.x264-GRP",
            "Show.S01E02.Telesyn.Girl.1080p.x264-GRP",
            // No canonical source to map these to.
            "Ajeeb Daastaans (2021) 480p HDRip x264 AAC ESub.mkv",
            "Yastreb.2011.XviD.IPTVRip.Files-x",
            "Movie.2026.1080p.Blurry.x264-GRP",
        ] {
            assert_eq!(source(title), None, "{title}");
        }
    }

    #[test]
    fn one_edit() {
        assert!(within_one_edit(b"telesyn", b"telesync"));
        assert!(within_one_edit(b"telesycn", b"telesync"));
        assert!(within_one_edit(b"telesynk", b"telesync"));
        assert!(within_one_edit(b"telessync", b"telesync"));
        assert!(!within_one_edit(b"telescope", b"telesync"));
        assert!(!within_one_edit(b"teleysnk", b"telesync"));
        assert!(!within_one_edit(b"screamer", b"screener"));
    }

    #[test]
    fn resolution_suffix() {
        assert_eq!(resolution_len("1080p"), Some(5));
        assert_eq!(resolution_len("720p50"), Some(6));
        assert_eq!(resolution_len("bdrip720p"), Some(4));
        assert_eq!(resolution_len("x264"), None);
        assert_eq!(resolution_len("h265"), None);
        assert_eq!(resolution_len("12p"), None);
    }
}
