//! `parse_quality` as it was before one Aho-Corasick pass replaced the
//! per-word substring searches. The new one must parse every title the same.

#![allow(dead_code)]

use super::*;

/// Check if a word appears with word boundaries in the title.
///
/// A literal scan rather than a regex: this runs ~90 times per parse and the
/// parse runs for every release × pattern pair in an RSS cycle, so compiling a
/// regex per call dominated scoring time. It also left tokens like `hdr10+`
/// unescaped, so they could never match.
///
/// A `.` in `word` matches any separator, as titles from indexers that swap dots
/// for spaces ("WEB DL", "H 264") relied on the old regex's wildcard `.`.
fn has_word(lower: &str, word: &str) -> bool {
    let Some(first_len) = word.chars().next().map(char::len_utf8) else {
        return false;
    };
    let (anchor, rest) = word
        .split_once('.')
        .map_or((word, None), |(a, _)| (a, Some(&word[a.len()..])));
    let mut from = 0;
    while let Some(offset) = lower[from..].find(anchor) {
        let start = from + offset;
        let Some(end) = rest.map_or(Some(start + anchor.len()), |rest| {
            match_separated(lower, start + anchor.len(), rest)
        }) else {
            from = start + first_len;
            continue;
        };
        let before_ok = lower[..start]
            .chars()
            .next_back()
            .is_none_or(|c| c.is_whitespace() || matches!(c, '.' | '-' | '[' | '('));
        let after_ok = lower[end..]
            .chars()
            .next()
            .is_none_or(|c| c.is_whitespace() || matches!(c, '.' | '-' | ']' | ')'));
        if before_ok && after_ok {
            return true;
        }
        // Step one char, not past the match, so overlapping occurrences are seen.
        from = start + first_len;
    }
    false
}

fn match_separated(lower: &str, mut pos: usize, pattern: &str) -> Option<usize> {
    for p in pattern.chars() {
        let c = lower[pos..].chars().next()?;
        let ok = if p == '.' {
            c.is_whitespace() || matches!(c, '.' | '-')
        } else {
            c == p
        };
        if !ok {
            return None;
        }
        pos += c.len_utf8();
    }
    Some(pos)
}

/// Find first matching item from a list of patterns.
fn find_match<'a>(lower: &str, patterns: &[(&[&str], &'a str)]) -> Option<&'a str> {
    for (aliases, canonical) in patterns {
        for alias in *aliases {
            if has_word(lower, alias) {
                return Some(canonical);
            }
        }
    }
    None
}

/// Detect audio codec with special handling for patterns with special characters.
fn detect_audio(lower: &str) -> Option<String> {
    // Object-based / Immersive (check first - they combine with base codecs)
    if has_word(lower, "atmos") || lower.contains("dolby.atmos") || lower.contains("truehd.atmos") {
        return Some("atmos".to_string());
    }
    if has_word(lower, "dts-x") || has_word(lower, "dtsx") || lower.contains("dts:x") {
        return Some("dts-x".to_string());
    }

    // Lossless
    if has_word(lower, "truehd") || has_word(lower, "true-hd") || has_word(lower, "mlp") {
        return Some("truehd".to_string());
    }
    if lower.contains("dts-hd.ma")
        || lower.contains("dts-hdma")
        || lower.contains("dtshdma")
        || lower.contains("dtshd.ma")
    {
        return Some("dts-hd.ma".to_string());
    }
    if lower.contains("dts-hd.hra")
        || lower.contains("dts-hd.hr")
        || lower.contains("dtshra")
        || lower.contains("dtshd.hra")
    {
        return Some("dts-hd.hra".to_string());
    }
    if has_word(lower, "pcm") || has_word(lower, "lpcm") {
        return Some("pcm".to_string());
    }
    if has_word(lower, "flac") {
        return Some("flac".to_string());
    }

    // DTS (basic, after DTS-X and DTS-HD checks)
    if has_word(lower, "dts") {
        return Some("dts".to_string());
    }

    // DD+ / EAC3 (check for + character specially since it's a regex metachar)
    if has_word(lower, "ddp")
        || has_word(lower, "eac3")
        || has_word(lower, "e-ac3")
        || lower.contains("dd+")
        || lower.contains("dd.plus")
        || lower.contains("ddplus")
    {
        return Some("ddp".to_string());
    }

    // DD / AC3
    if lower.contains("dd5.1")
        || lower.contains("dd7.1")
        || has_word(lower, "ac3")
        || has_word(lower, "ac-3")
        || lower.contains("dolby.digital")
    {
        return Some("dd".to_string());
    }

    // Standard lossy
    if has_word(lower, "aac") {
        return Some("aac".to_string());
    }
    if has_word(lower, "mp3") {
        return Some("mp3".to_string());
    }
    if has_word(lower, "opus") {
        return Some("opus".to_string());
    }
    if has_word(lower, "vorbis") || has_word(lower, "ogg") {
        return Some("vorbis".to_string());
    }
    if has_word(lower, "wma") {
        return Some("wma".to_string());
    }

    None
}

/// Parse quality information from a release title.
pub fn parse_quality(title: &str) -> ParsedQuality {
    // Some indexers (usenet especially) use `_` as the separator. Every token
    // matcher below only treats space/dot/dash/bracket as a boundary, so without
    // this `Movie_2026_1080p_TeleSync` parses as no resolution and no source —
    // which slips past rejected_sources entirely.
    let lower = title.to_lowercase().replace('_', ".");

    // Resolution — also match framerate-suffixed variants like 1080p50, 720p60
    let resolution = detect_resolution(&lower);

    // Source - use specialized detection for ambiguous patterns
    // Not part of what this reference pins down: both parsers share it.
    let source = super::salvage::refine_source(detect_source(&lower), &lower);

    // Streaming service
    let streaming_service = find_match(&lower, STREAMING_SERVICES).map(String::from);

    // Codec
    let codec = find_match(&lower, VIDEO_CODECS).map(String::from);

    // Audio (use specialized detection for patterns with special characters)
    let audio = detect_audio(&lower);

    // HDR detection (with priority ordering)
    let (is_hdr, is_hdr10, is_hdr10plus, is_dolby_vision, is_hlg) = detect_hdr(&lower);

    // 3D detection
    let (is_3d, format_3d) = detect_3d(&lower);

    // Release version flags
    let is_proper = has_word(&lower, "proper");
    let is_repack = has_word(&lower, "repack") || has_word(&lower, "rerip");
    let is_real = has_word(&lower, "real");
    let is_remux = has_word(&lower, "remux");

    // Release type flags
    let is_internal = has_word(&lower, "internal");
    let is_retail = has_word(&lower, "retail");
    let is_limited = has_word(&lower, "limited");

    // Fix flags
    let is_nfo_fix = has_word(&lower, "nfofix") || has_word(&lower, "nfo.fix");
    let is_sample_fix = has_word(&lower, "samplefix") || has_word(&lower, "sample.fix");
    let is_sync_fix = has_word(&lower, "syncfix")
        || has_word(&lower, "sync.fix")
        || has_word(&lower, "audiofix")
        || has_word(&lower, "audio.fix");
    let is_proof = has_word(&lower, "proof");
    let is_dirfix = has_word(&lower, "dirfix") || has_word(&lower, "dir.fix");

    // Subtitle/dub flags
    let is_dubbed = has_word(&lower, "dubbed") || has_word(&lower, "dub");
    let is_subbed = has_word(&lower, "subbed") || has_word(&lower, "subs");
    let is_hardcoded_subs = has_word(&lower, "hc")
        || has_word(&lower, "hardcoded")
        || has_word(&lower, "hardsub")
        || has_word(&lower, "hardsubs")
        || has_word(&lower, "korsub") // Korean subs burned in
        || has_word(&lower, "chtsub") // Chinese traditional subs
        || has_word(&lower, "subtitulado")
        || has_word(&lower, "subtitulada")
        || has_word(&lower, "vose");

    // Advanced audio flags
    let is_atmos = has_word(&lower, "atmos") || has_word(&lower, "dolby.atmos");
    let is_dtsx =
        has_word(&lower, "dts-x") || has_word(&lower, "dtsx") || has_word(&lower, "dts:x");

    // Nuked/obfuscated
    let is_nuked = has_word(&lower, "nuked") || has_word(&lower, "nuke");
    let is_obfuscated = OBFUSCATED_RE.is_match(&lower);

    // Language
    let language = crate::language::title_language(|w| has_word(&lower, w)).map(String::from);

    // Release group
    let release_group = RELEASE_GROUP_RE
        .captures(title)
        .and_then(|caps| caps.get(1))
        .map(|m| m.as_str().to_string());

    // Version
    let version = VERSION_RE
        .captures(&lower)
        .and_then(|caps| caps.get(1))
        .and_then(|m| m.as_str().parse().ok());

    ParsedQuality {
        resolution,
        source,
        streaming_service,
        codec,
        audio,
        language,
        is_proper,
        is_repack,
        is_real,
        is_remux,
        is_internal,
        is_retail,
        is_limited,
        is_nfo_fix,
        is_sample_fix,
        is_sync_fix,
        is_proof,
        is_dirfix,
        is_dubbed,
        is_subbed,
        is_hardcoded_subs,
        is_hdr,
        is_hdr10,
        is_hdr10plus,
        is_dolby_vision,
        is_hlg,
        is_atmos,
        is_dtsx,
        is_3d,
        format_3d,
        is_nuked,
        is_obfuscated,
        release_group,
        version,
    }
}

/// Detect resolution, handling framerate-suffixed variants like `1080p50`, `720p60`.
///
/// Standard aliases (e.g., `1080p`, `fullhd`, `4k`) use normal word-boundary matching.
/// For `p`/`i`-suffixed resolutions we also accept an optional framerate (`\d{2,3}`)
/// immediately after the `p`/`i` — e.g., `1080p50` is recognised as `1080p`.
fn detect_resolution(lower: &str) -> Option<String> {
    // First try exact word-boundary matches (handles aliases like "fullhd", "4k", "uhd", "ntsc")
    if let Some((_, canonical, _)) = RESOLUTIONS
        .iter()
        .find(|(aliases, _, _)| aliases.iter().any(|a| has_word(lower, a)))
    {
        return Some(canonical.to_string());
    }

    // Framerate-suffixed fallback: match e.g. 1080p50, 720p60, 576i25
    // Pattern: word-boundary, digits, p or i, 2-3 digit framerate, word-boundary
    static RES_FPS_RE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(r"(?:^|[\s.\-\[\(])(\d{3,4}[pi])\d{2,3}(?:[\s.\-\]\)]|$)").unwrap()
    });
    if let Some(caps) = RES_FPS_RE.captures(lower) {
        let base = caps.get(1).unwrap().as_str(); // e.g. "1080p"
        return RESOLUTIONS
            .iter()
            .find(|(aliases, _, _)| aliases.contains(&base))
            .map(|(_, canonical, _)| canonical.to_string());
    }

    None
}

/// Detect video source with special handling for ambiguous patterns.
fn detect_source(lower: &str) -> Option<String> {
    // Check remux first (highest quality indicator)
    if has_word(lower, "remux") {
        return Some("remux".to_string());
    }

    // Check explicit bluray patterns
    if has_word(lower, "bluray")
        || has_word(lower, "blu-ray")
        || has_word(lower, "bdrip")
        || has_word(lower, "brrip")
        || has_word(lower, "bd25")
        || has_word(lower, "bd50")
    {
        return Some("bluray".to_string());
    }

    // Unambiguous pre-release markers win over WEB/TV/DVD tags: mislabelled
    // uploads like `Movie.2026.1080p.TS.WEB.DL` or `movie.2026.web.CAM` are
    // still a TS/CAM, and first-match-wins on WEB let them past rejected_sources.
    // Only the tag region after the year/episode/resolution is checked, so a
    // title word (`Cam.2018.1080p.WEB-DL`) can't trip it. A trailing `.ts` is a
    // transport-stream file extension, not a source tag.
    let body = lower.strip_suffix(".ts").unwrap_or(lower);
    if let Some(m) = TAG_REGION_RE.find(body) {
        let tags = &body[m.start()..];
        if TS_RE.is_match(tags) {
            return Some("telesync".to_string());
        }
        if has_word(tags, "cam") || has_word(tags, "camrip") || has_word(tags, "hdcam") {
            return Some("cam".to_string());
        }
        if has_word(tags, "telecine") {
            return Some("telecine".to_string());
        }
    }

    // WEB sources
    if has_word(lower, "web-dl") || has_word(lower, "webdl") || has_word(lower, "web.dl") {
        return Some("web-dl".to_string());
    }
    if has_word(lower, "webrip") || has_word(lower, "web-rip") || has_word(lower, "web.rip") {
        return Some("webrip".to_string());
    }
    // Generic "web" - only match as word boundary
    if has_word(lower, "web") {
        return Some("webrip".to_string());
    }

    // TV sources
    if has_word(lower, "hdtv") || has_word(lower, "hd-tv") {
        return Some("hdtv".to_string());
    }
    if has_word(lower, "pdtv") {
        return Some("pdtv".to_string());
    }
    if has_word(lower, "dsr") || has_word(lower, "dsrip") {
        return Some("dsr".to_string());
    }
    if has_word(lower, "sdtv")
        || has_word(lower, "tvrip")
        || has_word(lower, "tv-rip")
        || has_word(lower, "sd-tv")
    {
        return Some("sdtv".to_string());
    }
    if has_word(lower, "satrip") || has_word(lower, "satellite") {
        return Some("satellite".to_string());
    }

    // DVD sources
    if has_word(lower, "dvdrip") || has_word(lower, "dvd-rip") {
        return Some("dvdrip".to_string());
    }
    if has_word(lower, "dvdr")
        || has_word(lower, "dvd-r")
        || has_word(lower, "dvd5")
        || has_word(lower, "dvd9")
    {
        return Some("dvdr".to_string());
    }
    if has_word(lower, "dvdscr") || has_word(lower, "dvd-screener") {
        return Some("dvdscr".to_string());
    }
    // Generic DVD
    if has_word(lower, "dvd") {
        return Some("dvd".to_string());
    }

    // Pre-release sources
    if has_word(lower, "bdscr") || has_word(lower, "bluray-screener") {
        return Some("bdscr".to_string());
    }
    if has_word(lower, "screener") || has_word(lower, "scr") {
        return Some("screener".to_string());
    }
    if has_word(lower, "r5") {
        return Some("r5".to_string());
    }
    if has_word(lower, "telecine") {
        return Some("telecine".to_string());
    }
    // TC with word boundary (avoids false positives)
    if has_word(lower, "tc") && !lower.contains("etc") {
        return Some("telecine".to_string());
    }
    // TS/Telesync - use special regex to avoid false positives
    if TS_RE.is_match(lower) {
        return Some("telesync".to_string());
    }
    // CAM with word boundary
    if has_word(lower, "cam") || has_word(lower, "camrip") || has_word(lower, "hdcam") {
        return Some("cam".to_string());
    }
    if has_word(lower, "workprint") || has_word(lower, "wp") {
        return Some("workprint".to_string());
    }
    if has_word(lower, "ppvrip") || has_word(lower, "ppv") {
        return Some("ppv".to_string());
    }
    if has_word(lower, "vod") || has_word(lower, "vodrip") {
        return Some("vod".to_string());
    }

    // HD-DVD
    if has_word(lower, "hddvd") || has_word(lower, "hd-dvd") {
        return Some("hddvd".to_string());
    }

    None
}

/// Detect HDR format with priority ordering.
fn detect_hdr(lower: &str) -> (bool, bool, bool, bool, bool) {
    let is_dolby_vision = has_word(lower, "dv")
        || has_word(lower, "dovi")
        || has_word(lower, "dolby.vision")
        || has_word(lower, "dolbyvision");

    let is_hdr10plus = has_word(lower, "hdr10plus")
        || has_word(lower, "hdr10+")
        || lower.contains("hdr10.plus")
        || lower.contains("hdr.10.plus");

    let is_hdr10 = (has_word(lower, "hdr10") || lower.contains("hdr.10")) && !is_hdr10plus; // Don't double-count HDR10+

    let is_hlg = has_word(lower, "hlg") || lower.contains("hybrid.log.gamma");

    let is_hdr = has_word(lower, "hdr") || is_hdr10 || is_hdr10plus || is_dolby_vision || is_hlg;

    (is_hdr, is_hdr10, is_hdr10plus, is_dolby_vision, is_hlg)
}

/// Detect 3D format.
fn detect_3d(lower: &str) -> (bool, Option<String>) {
    for (aliases, canonical) in FORMATS_3D {
        for alias in *aliases {
            if has_word(lower, alias) {
                return (true, Some(canonical.to_string()));
            }
        }
    }
    (false, None)
}

mod equivalence {
    use proptest::prelude::*;

    fn assert_same(title: &str) {
        let old = serde_json::to_value(super::parse_quality(title)).unwrap();
        let new = serde_json::to_value(super::super::parse_quality(title)).unwrap();
        assert_eq!(old, new, "{title:?}");
    }

    fn read_lines(path: &std::path::Path) -> Vec<String> {
        use std::io::BufRead;
        let file = std::fs::File::open(path).expect("open title corpus");
        std::io::BufReader::new(flate2::read::GzDecoder::new(file))
            .lines()
            .map(|l| l.expect("read title corpus"))
            .collect()
    }

    /// The title corpora and every release title in the capture. Needs
    /// `KOBLOAD_TITLE_CORPUS=$PWD/data/scoring-corpus` and
    /// `KOBLOAD_SCORING_CORPUS=$PWD/data/scoring-capture`, in release mode.
    #[test]
    fn corpus_titles_parse_like_the_reference() {
        let (Some(titles), Some(capture)) = (
            std::env::var_os("KOBLOAD_TITLE_CORPUS").map(std::path::PathBuf::from),
            std::env::var_os("KOBLOAD_SCORING_CORPUS").map(std::path::PathBuf::from),
        ) else {
            eprintln!("KOBLOAD_TITLE_CORPUS/KOBLOAD_SCORING_CORPUS not set; skipping");
            return;
        };
        let mut all: std::collections::BTreeSet<String> =
            ["titles-matched.txt.gz", "titles-dht-sample.txt.gz"]
                .iter()
                .flat_map(|f| read_lines(&titles.join(f)))
                .collect();
        all.extend(
            crate::watch::capture::read_capture(&capture)
                .expect("read capture")
                .into_iter()
                .flat_map(|f| f.fetch.releases.into_iter().map(|r| r.title.clone())),
        );
        for title in &all {
            assert_same(title);
        }
        eprintln!("{} titles parse the same", all.len());
    }

    fn token() -> impl Strategy<Value = String> {
        prop_oneof![
            "[A-Za-z]{1,8}",
            "[0-9]{1,4}",
            "(1080p|720p|2160p|480p|4k|uhd|1080p50|576i25|fullhd|ntsc)",
            "(web|web-dl|web.dl|webrip|hdtv|bluray|blu-ray|remux|dvdrip|ts|hdts|cam|tc|telecine|scr|r5|wp|ppv|vod)",
            "(x264|x265|h264|h.264|hevc|avc|av1|xvid|aac|ac3|ddp|dd\\+|dts|dts-hd|dts:x|truehd|atmos|flac|opus)",
            "(hdr|hdr10|hdr10\\+|hdr10plus|dv|dovi|dolby.vision|hlg|3d|sbs|hsbs|ou)",
            "(proper|repack|real|internal|nuked|subbed|dubbed|hc|korsub|subtitulado|vose|multi|dual|english|vostfr|nf|amzn|dsnp|atvp)",
            "[\\[(][A-Za-z0-9 .-]{1,8}[\\])]",
            "(Café|Pokémon|日本|C\\+\\+|Rock&Roll|It's|ÆON|straße)",
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(20_000))]

        #[test]
        fn random_titles_parse_like_the_reference(
            tokens in prop::collection::vec(token(), 1..14),
            seps in prop::collection::vec(prop_oneof![Just("."), Just(" "), Just("_"), Just("-")], 14),
            tail in "(-[A-Za-z0-9]{2,8}(\\.mkv|\\.ts)?)?",
            upper in any::<bool>(),
        ) {
            let mut title = String::new();
            for (i, t) in tokens.iter().enumerate() {
                if i > 0 {
                    title.push_str(seps[i]);
                }
                title.push_str(t);
            }
            title.push_str(&tail);
            if upper {
                title = title.to_uppercase();
            }
            assert_same(&title);
        }
    }
}
