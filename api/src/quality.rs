//! Quality parsing and scoring.
//!
//! Comprehensive release quality detection supporting all common scene conventions:
//! - Video sources (Blu-ray, WEB-DL, HDTV, etc.)
//! - Video codecs (x264, x265/HEVC, AV1, etc.)
//! - Audio codecs (TrueHD, Atmos, DTS:X, etc.)
//! - HDR formats (HDR10, Dolby Vision, HLG)
//! - Release flags (PROPER, REPACK, INTERNAL, etc.)
//! - Streaming service tags (AMZN, NF, DSNP, etc.)
//! - Languages and subtitle indicators

use regex::Regex;
use std::sync::LazyLock;

pub use nats_jobs::ParsedQuality;
use nats_jobs::QualityOverrides;

use crate::repositories::quality_profiles::QualityProfileRow;
use crate::trash_filters::TrashFilterSet;

#[derive(Debug, Clone)]
pub struct QualityPreferences {
    pub min_resolution: Option<String>,
    pub max_resolution: Option<String>,
    pub preferred_sources: Vec<String>,
    pub preferred_codecs: Vec<String>,
    pub preferred_audio: Vec<String>,
    pub preferred_languages: Vec<String>,
    pub rejected_sources: Vec<String>,
    pub rejected_languages: Vec<String>,
    pub rejected_terms: Vec<String>,
    pub reject_hardcoded_subs: bool,
    pub reject_remux: bool,
}

#[derive(Debug, Clone)]
pub struct QualityScore {
    pub score: f64,
    pub parsed: ParsedQuality,
    /// Whether this release should be hard-rejected based on quality profile constraints.
    pub rejected: bool,
    /// All quality rejection reasons (a release can violate multiple rules).
    pub reject_reasons: Vec<nats_jobs::QualityRejectReason>,
    /// Itemized scoring breakdown for decision log instrumentation.
    pub breakdown: crate::decision_log::QualityScoreBreakdown,
}

impl QualityScore {
    /// Primary rejection reason (first in the list), for backwards-compatible display.
    pub fn primary_reject_reason(&self) -> Option<nats_jobs::QualityRejectReason> {
        self.reject_reasons.first().copied()
    }
}

/// Streaming service tags.
const STREAMING_SERVICES: &[(&[&str], &str)] = &[
    // Major US services
    (&["amzn", "amazon", "amazonhd"], "amazon"),
    (&["nf", "netflix", "netflixhd"], "netflix"),
    (
        &["dsnp", "dnsp", "disney", "disneyplus", "disney+"],
        "disney+",
    ),
    (&["atvp", "atv", "aptv", "appletv", "appletv+"], "appletv"),
    (&["hmax", "hbo", "hbomax"], "hbomax"),
    (&["hulu"], "hulu"),
    (&["pcok", "peacock"], "peacock"),
    (&["pmtp", "paramount", "paramount+"], "paramount+"),
    // International
    (&["stan"], "stan"),                 // Australia
    (&["crav", "crave"], "crave"),       // Canada
    (&["now", "nowtv"], "nowtv"),        // UK
    (&["iplayer", "bbc"], "bbc"),        // UK
    (&["all4", "channel4"], "channel4"), // UK
    (&["hotstar"], "hotstar"),           // India
    (&["zee5"], "zee5"),                 // India
    // Purchase/rental platforms
    (&["it", "itunes"], "itunes"),
    (&["vudu"], "vudu"),
    (&["ma", "moviesanywhere"], "moviesanywhere"),
    (&["gplay", "googleplay"], "googleplay"),
    // Specialty
    (&["bcore", "braviacore"], "braviacore"),
    (&["red", "youtube", "ytred"], "youtube"),
    (&["roku"], "roku"),
    (&["criterion", "cc"], "criterion"),
    (&["mubi"], "mubi"),
    (&["shout", "shoutfactory"], "shoutfactory"),
    (&["dcu", "dcuniverse"], "dcuniverse"),
    (&["crunchyroll", "cr"], "crunchyroll"),
    (&["funimation", "funi"], "funimation"),
    (&["hidive"], "hidive"),
    (&["vrv"], "vrv"),
    (&["showtime", "sho"], "showtime"),
    (&["starz"], "starz"),
    (&["epix"], "epix"),
    (&["mgm"], "mgm"),
    (&["tubi"], "tubi"),
    (&["pluto"], "pluto"),
    (&["curiosity", "curiositystream"], "curiositystream"),
    (&["nebula"], "nebula"),
    (&["dropout"], "dropout"),
];

// =============================================================================
// Video Codecs
// =============================================================================

const VIDEO_CODECS: &[(&[&str], &str)] = &[
    // Modern codecs
    (&["x265", "h265", "hevc", "h.265"], "hevc"),
    (&["av1"], "av1"),
    (&["vp9"], "vp9"),
    // Legacy codecs
    (&["x264", "h264", "avc", "h.264"], "avc"),
    (&["vc-1", "vc1"], "vc1"),
    (&["mpeg2", "mpeg-2", "h262"], "mpeg2"),
    (&["xvid", "divx", "mpeg4", "mpeg-4"], "mpeg4"),
    // Lossless
    (&["huffyuv", "huffy"], "huffyuv"),
    (&["ffv1"], "ffv1"),
];

// =============================================================================
// Resolutions
// =============================================================================

const RESOLUTIONS: &[(&[&str], &str, i32)] = &[
    // UHD+
    (&["4320p", "8k"], "4320p", 4320),
    (&["2160p", "4k", "uhd"], "2160p", 2160),
    // HD
    (&["1080p", "fullhd", "fhd"], "1080p", 1080),
    (&["1080i"], "1080i", 1080),
    (&["720p", "hd"], "720p", 720),
    // SD
    (&["576p", "pal"], "576p", 576),
    (&["576i"], "576i", 576),
    (&["540p"], "540p", 540),
    (&["480p", "ntsc", "sd"], "480p", 480),
    (&["480i"], "480i", 480),
    (&["360p"], "360p", 360),
];

// =============================================================================
// 3D Formats
// =============================================================================

const FORMATS_3D: &[(&[&str], &str)] = &[
    // Side-by-side
    (&["hsbs", "sbs", "half.sbs"], "hsbs"),
    (&["fsbs", "full.sbs"], "fsbs"),
    // Over-under (top and bottom)
    (&["htab", "hou", "half.ou", "half.tab"], "htab"),
    (&["ftab", "full.ou", "full.tab"], "ftab"),
    // Blu-ray 3D
    (&["mvc", "3dbd", "3d.bd"], "mvc"),
    // Generic 3D
    (&["3d"], "3d"),
];

// =============================================================================
// Languages
// =============================================================================

const LANGUAGES: &[(&str, &str)] = &[
    // Multi-language
    ("multi", "multi"),
    ("dual", "multi"),
    // Full names
    ("english", "english"),
    ("french", "french"),
    ("german", "german"),
    ("spanish", "spanish"),
    ("italian", "italian"),
    ("russian", "russian"),
    ("korean", "korean"),
    ("japanese", "japanese"),
    ("chinese", "chinese"),
    ("mandarin", "chinese"),
    ("cantonese", "cantonese"),
    ("hindi", "hindi"),
    ("arabic", "arabic"),
    ("portuguese", "portuguese"),
    ("dutch", "dutch"),
    ("swedish", "swedish"),
    ("danish", "danish"),
    ("norwegian", "norwegian"),
    ("finnish", "finnish"),
    ("polish", "polish"),
    ("czech", "czech"),
    ("hungarian", "hungarian"),
    ("turkish", "turkish"),
    ("thai", "thai"),
    ("vietnamese", "vietnamese"),
    ("indonesian", "indonesian"),
    ("malay", "malay"),
    ("tagalog", "tagalog"),
    ("hebrew", "hebrew"),
    ("greek", "greek"),
    ("romanian", "romanian"),
    ("bulgarian", "bulgarian"),
    ("ukrainian", "ukrainian"),
    // Alternate full names
    ("latino", "spanish"),
    ("castellano", "spanish"),
    ("flemish", "dutch"),
    // ISO 639-2/B abbreviations
    ("eng", "english"),
    ("ita", "italian"),
    ("fre", "french"),
    ("fra", "french"),
    ("ger", "german"),
    ("deu", "german"),
    ("spa", "spanish"),
    ("rus", "russian"),
    ("kor", "korean"),
    ("jpn", "japanese"),
    ("chn", "chinese"),
    ("chi", "chinese"),
    ("hin", "hindi"),
    ("ara", "arabic"),
    ("por", "portuguese"),
    ("nld", "dutch"),
    ("swe", "swedish"),
    ("dan", "danish"),
    ("nor", "norwegian"),
    ("fin", "finnish"),
    ("pol", "polish"),
    ("ces", "czech"),
    ("cze", "czech"),
    ("hun", "hungarian"),
    ("tur", "turkish"),
    ("tha", "thai"),
    ("vie", "vietnamese"),
    ("ind", "indonesian"),
    ("msa", "malay"),
    ("tgl", "tagalog"),
    ("heb", "hebrew"),
    ("ell", "greek"),
    ("ron", "romanian"),
    ("bul", "bulgarian"),
    ("ukr", "ukrainian"),
    // Scene-specific tags
    ("truefrench", "french"),
    ("vostfr", "french"),
    ("subfrench", "french"),
    ("fastsub", "french"),
];

// =============================================================================
// Regexes
// =============================================================================

/// Word-boundary regex for TS source detection (avoids false positives like "Monsters").
static TS_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:^|[\s.\-\[\(])(?:HDTS|TELESYNC|TS)(?:[\s.\-\]\)]|$)").unwrap()
});

// Start of the tag region: the first year, SxxEyy or resolution token.
static TAG_REGION_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:^|[\s.\-\[\(])(?:(?:19|20)\d{2}|s\d{1,2}e\d{1,3}|\d{3,4}[pi])(?:[\s.\-\]\)]|$)")
        .unwrap()
});

/// Release group pattern: -GroupName at end of title (before extension if any).
static RELEASE_GROUP_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"-([A-Za-z0-9]+)(?:\.[a-z]{2,4})?$").unwrap());

/// Version pattern: v2, v3, VERSION2, etc.
static VERSION_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:^|[\s.\-\[\(])(?:v|version)(\d)(?:[\s.\-\]\)]|$)").unwrap()
});

/// Obfuscation indicators
static OBFUSCATED_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)(?:-obfuscated|-nzbgeek|-scrambled|^[a-f0-9]{32}$)").unwrap()
});

// =============================================================================
// Scoring Functions
// =============================================================================

/// Check if a rejected term matches within a release title, ignoring the
/// separator style used by the release.
///
/// Release titles vary in how they join words (`Audio.Description`,
/// `Audio Description`, `Audio_Description`, `Audio-Description`), so both the
/// title and the term are normalized: every run of separator characters
/// (`.`, `_`, `-`, space) collapses to a single space. A term therefore matches
/// regardless of which separator the release used, and a term written without
/// separators (`SignZone`) still matches only the unseparated form.
///
/// Matching is token-bounded on both ends — the character before and after the
/// match must not be alphanumeric — so `ASL` does not match inside `CASTLE`,
/// and `3D` does not match inside `X3D`.
fn rejected_term_matches(title_lower: &str, term_lower: &str) -> bool {
    let title_norm = normalize_separators(title_lower);
    let term_norm = normalize_separators(term_lower);

    let term_bytes = term_norm.as_bytes();
    let title_bytes = title_norm.as_bytes();
    let tlen = term_bytes.len();
    if tlen == 0 || tlen > title_bytes.len() {
        return false;
    }
    for start in 0..=(title_bytes.len() - tlen) {
        if &title_bytes[start..start + tlen] != term_bytes {
            continue;
        }
        let before_ok = start == 0 || !title_bytes[start - 1].is_ascii_alphanumeric();
        let after_ok =
            start + tlen == title_bytes.len() || !title_bytes[start + tlen].is_ascii_alphanumeric();
        if before_ok && after_ok {
            return true;
        }
    }
    false
}

/// Collapse every run of separator characters (`.`, `_`, `-`, space) into a
/// single space, and trim leading/trailing separators.
///
/// `".Audio..Description."` and `"_audio description_"` both become
/// `"audio description"`, so separator style cannot defeat a term match.
fn normalize_separators(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut pending_sep = false;
    for ch in input.chars() {
        if matches!(ch, '.' | '_' | '-' | ' ') {
            // Only emit a separator once we know a non-separator follows,
            // which also drops leading and trailing runs.
            pending_sep = !out.is_empty();
        } else {
            if pending_sep {
                out.push(' ');
                pending_sep = false;
            }
            out.push(ch);
        }
    }
    out
}

/// Check if a profile value matches a parsed value, accounting for aliases.
/// Profile UIs use human-friendly names (e.g. "x265", "ts", "dd") while the
/// parser normalizes to canonical forms (e.g. "hevc", "telesync", "dd").
fn profile_value_matches(profile_val: &str, parsed_val: &str, aliases: &[(&[&str], &str)]) -> bool {
    if profile_val.eq_ignore_ascii_case(parsed_val) {
        return true;
    }
    let lower = profile_val.to_ascii_lowercase();
    for (names, canonical) in aliases {
        if names.iter().any(|n| n.eq_ignore_ascii_case(&lower)) {
            return canonical.eq_ignore_ascii_case(parsed_val);
        }
    }
    false
}

/// Source aliases: profile name → parser canonical form
const SOURCE_ALIASES: &[(&[&str], &str)] = &[
    (&["ts", "hdts"], "telesync"),
    (&["tc"], "telecine"),
    (&["webdl"], "web-dl"),
];

/// Codec aliases: profile name → parser canonical form
const CODEC_ALIASES: &[(&[&str], &str)] = &[
    (&["x265", "h265", "h.265"], "hevc"),
    (&["x264", "h264", "h.264"], "avc"),
];

/// Audio aliases: profile name → parser canonical form
const AUDIO_ALIASES: &[(&[&str], &str)] = &[
    (&["dd+", "eac3"], "ddp"),
    (&["ac3"], "dd"),
    (&["dts-hd"], "dts-hd.ma"), // DTS-HD.MA is the common lossless DTS variant
    (&["dts-hd.hra"], "dts-hd.hra"),
];

/// Check if a profile language matches a parsed language.
/// Normalizes both sides through the LANGUAGES table so "eng" matches "english",
/// "ita" matches "italian", etc.
fn language_matches(profile_lang: &str, parsed_lang: &str) -> bool {
    if profile_lang.eq_ignore_ascii_case(parsed_lang) {
        return true;
    }
    // Normalize profile value through LANGUAGES table
    let profile_canonical = LANGUAGES
        .iter()
        .find(|(word, _)| word.eq_ignore_ascii_case(profile_lang))
        .map(|(_, canonical)| *canonical);
    let parsed_canonical = LANGUAGES
        .iter()
        .find(|(word, _)| word.eq_ignore_ascii_case(parsed_lang))
        .map(|(_, canonical)| *canonical);
    match (profile_canonical, parsed_canonical) {
        (Some(pc), Some(rc)) => pc.eq_ignore_ascii_case(rc),
        _ => false,
    }
}

pub fn score_quality(
    title: &str,
    profile: Option<&QualityProfileRow>,
    overrides: Option<&QualityOverrides>,
    trash_filters: Option<&TrashFilterSet>,
) -> QualityScore {
    let parsed = parse_quality(title);
    let prefs = merge_preferences(profile, overrides);

    use nats_jobs::QualityRejectReason;

    let mut score = 0.0;
    let mut rejected = false;
    let mut reject_reasons: Vec<QualityRejectReason> = Vec::new();
    let mut bd = crate::decision_log::QualityScoreBreakdown::default();

    // Resolution scoring
    if let Some(min) = prefs.min_resolution.as_deref() {
        match (
            parse_resolution(min),
            parsed.resolution.as_deref().and_then(parse_resolution),
        ) {
            (Some(min_val), Some(val)) => {
                if val < min_val {
                    let v = -0.5;
                    score += v;
                    bd.min_resolution_check = v;
                    rejected = true;
                    reject_reasons.push(QualityRejectReason::BelowMinResolution);
                } else {
                    let v = 0.15;
                    score += v;
                    bd.min_resolution_check = v;
                }
            }
            (Some(_), None) => {
                let v = -0.2;
                score += v;
                bd.min_resolution_check = v;
            }
            _ => {}
        }
    }

    if let Some(max) = prefs.max_resolution.as_deref()
        && let Some(max_val) = parse_resolution(max)
        && let Some(val) = parsed.resolution.as_deref().and_then(parse_resolution)
    {
        if val > max_val {
            let v = -0.3;
            score += v;
            bd.max_resolution_check = v;
            rejected = true;
            reject_reasons.push(QualityRejectReason::ExceedsMaxResolution);
        } else {
            let v = 0.1;
            score += v;
            bd.max_resolution_check = v;
        }
    }

    // Resolution tier bonus (unconditional) — ensures higher resolutions score higher
    if let Some(res) = parsed.resolution.as_deref()
        && let Some(height) = parse_resolution(res)
    {
        let v = match height {
            h if h >= 2160 => 0.40,
            h if h >= 1080 => 0.25,
            h if h >= 720 => 0.10,
            _ => 0.0,
        };
        score += v;
        bd.resolution_tier = v;
    }

    // Preferred source match
    if let Some(source) = parsed.source.as_deref()
        && prefs
            .preferred_sources
            .iter()
            .any(|pref| profile_value_matches(pref, source, SOURCE_ALIASES))
    {
        let v = 0.2;
        score += v;
        bd.preferred_source = v;
    }

    // Rejected source penalty
    if let Some(source) = parsed.source.as_deref()
        && prefs
            .rejected_sources
            .iter()
            .any(|rej| profile_value_matches(rej, source, SOURCE_ALIASES))
    {
        let v = -1.0;
        score += v;
        bd.rejected_source = v;
        rejected = true;
        reject_reasons.push(QualityRejectReason::RejectedSource);
    }

    // Preferred codec match
    if let Some(codec) = parsed.codec.as_deref()
        && prefs
            .preferred_codecs
            .iter()
            .any(|pref| profile_value_matches(pref, codec, CODEC_ALIASES))
    {
        let v = 0.15;
        score += v;
        bd.preferred_codec = v;
    }

    // Preferred audio match
    // Atmos is a DD+ superset, DTS:X is a DTS-HD superset — match against base codec prefs too
    let audio_matches_preferred = if let Some(audio) = parsed.audio.as_deref() {
        prefs
            .preferred_audio
            .iter()
            .any(|pref| profile_value_matches(pref, audio, AUDIO_ALIASES))
    } else {
        false
    };
    let immersive_matches_preferred = if parsed.is_atmos {
        prefs.preferred_audio.iter().any(|pref| {
            let p = pref.to_ascii_lowercase();
            p == "ddp" || p == "dd+" || p == "dd" || p == "eac3" || p == "ac3"
        })
    } else if parsed.is_dtsx {
        prefs.preferred_audio.iter().any(|pref| {
            let p = pref.to_ascii_lowercase();
            p == "dts-hd" || p == "dts-hd.ma" || p == "dts"
        })
    } else {
        false
    };
    if audio_matches_preferred || immersive_matches_preferred {
        let v = 0.05;
        score += v;
        bd.preferred_audio = v;
    }

    // Hardcoded subs penalty
    if parsed.is_hardcoded_subs && prefs.reject_hardcoded_subs {
        let v = -0.5;
        score += v;
        bd.hardcoded_subs = v;
        rejected = true;
        reject_reasons.push(QualityRejectReason::RejectedHardcodedSubs);
    }

    // Remux rejection penalty
    if parsed.is_remux && prefs.reject_remux {
        let v = -1.0;
        score += v;
        bd.remux_rejection = v;
        rejected = true;
        reject_reasons.push(QualityRejectReason::RejectedRemux);
    }

    // Proper/repack bonus (always good)
    if parsed.is_proper || parsed.is_repack {
        let v = 0.1;
        score += v;
        bd.proper_repack = v;
    }

    // REAL bonus (replaces fake release)
    if parsed.is_real {
        let v = 0.15;
        score += v;
        bd.real = v;
    }

    // Remux bonus (always a quality signal)
    if parsed.is_remux {
        let v = 0.15;
        score += v;
        bd.remux_bonus = v;
    }

    // HDR bonus
    if parsed.is_dolby_vision {
        let v = 0.10;
        score += v;
        bd.dolby_vision = v;
    } else if parsed.is_hdr10plus {
        let v = 0.08;
        score += v;
        bd.hdr10plus = v;
    } else if parsed.is_hdr10 || parsed.is_hdr {
        let v = 0.06;
        score += v;
        bd.hdr10 = v;
    }

    // Atmos/DTS:X bonus (tiebreaker, not upgrade-worthy on its own)
    if parsed.is_atmos || parsed.is_dtsx {
        let v = 0.03;
        score += v;
        bd.atmos_dtsx = v;
    }

    // Internal releases often higher quality (tiebreaker)
    if parsed.is_internal {
        let v = 0.02;
        score += v;
        bd.internal = v;
    }

    // Nuked releases are bad
    if parsed.is_nuked {
        let v = -1.0;
        score += v;
        bd.nuked = v;
        rejected = true;
        reject_reasons.push(QualityRejectReason::Nuked);
    }

    // Language preference = allowlist. If set and release has a detected language not in
    // the list, hard reject. Releases with no detected language pass through (English
    // releases typically aren't tagged with a language).
    // Multi/dual releases include multiple languages (always including English) — treat
    // as compatible with any preferred language but with a smaller bonus so confirmed
    // single-language matches rank higher.
    if !prefs.preferred_languages.is_empty()
        && let Some(lang) = parsed.language.as_deref()
    {
        if prefs
            .preferred_languages
            .iter()
            .any(|pref| language_matches(pref, lang))
        {
            let v = 0.2;
            score += v;
            bd.language = v;
        } else if lang == "multi" || lang == "dual" {
            let v = 0.05;
            score += v;
            bd.language = v;
        } else {
            let v = -1.0;
            score += v;
            bd.language = v;
            rejected = true;
            reject_reasons.push(QualityRejectReason::LanguageNotPreferred);
        }
    }

    // Explicit rejected languages — hard reject even if preferred_languages is empty
    if let Some(lang) = parsed.language.as_deref()
        && prefs
            .rejected_languages
            .iter()
            .any(|rej| language_matches(rej, lang))
    {
        let v = -1.0;
        score += v;
        bd.rejected_language = v;
        rejected = true;
        reject_reasons.push(QualityRejectReason::RejectedLanguage);
    }

    // Rejected terms — case-insensitive token-boundary match against raw release title.
    // Terms containing non-alphanumeric chars (e.g., ".Signed.", "Sign.Zone") are matched
    // literally since those chars act as natural delimiters. Plain alphanumeric terms
    // (e.g., "ASL", "3D") are matched only at token boundaries (dots, dashes, underscores,
    // spaces, or start/end of string) to avoid false positives like "CASTLE" matching "ASL".
    let title_lower = title.to_ascii_lowercase();
    if prefs
        .rejected_terms
        .iter()
        .any(|term| rejected_term_matches(&title_lower, &term.to_ascii_lowercase()))
    {
        let v = -1.0;
        score += v;
        bd.rejected_term = v;
        rejected = true;
        reject_reasons.push(QualityRejectReason::RejectedTerm);
    }

    // TRaSH Guides community filters — global blocklist + tier bonuses
    // Some categories (no_group, x265_hd) are soft-penalized rather than
    // hard-rejected so older/niche content with only these releases still surfaces.
    let use_trash = profile.map(|p| p.use_trash_filters).unwrap_or(true);
    if use_trash
        && !rejected
        && let Some(filters) = trash_filters
        && let Some(category) = filters.check_blocklist(title, parsed.release_group.as_deref())
    {
        let soft_penalty = matches!(category, "no_group" | "x265_hd");
        if soft_penalty {
            let v = -0.3;
            score += v;
            bd.trash_filter = v;
        } else {
            let v = -1.0;
            score += v;
            bd.trash_filter = v;
            rejected = true;
        }
        reject_reasons.push(match category {
            "lq" | "lq_title" => QualityRejectReason::TrashLq,
            "upscaled" => QualityRejectReason::TrashUpscaled,
            "br_disk" => QualityRejectReason::TrashBrDisk,
            "bad_dual" => QualityRejectReason::TrashBadDual,
            "line_mic" => QualityRejectReason::TrashLineMic,
            "obfuscated" => QualityRejectReason::TrashObfuscated,
            "retags" => QualityRejectReason::TrashRetags,
            "x265_hd" => QualityRejectReason::TrashX265Hd,
            "extras" => QualityRejectReason::TrashExtras,
            "no_group" => QualityRejectReason::TrashNoGroup,
            "anime_lq" => QualityRejectReason::TrashAnimeLq,
            _ => QualityRejectReason::TrashFilter,
        });
    }

    // TRaSH tier bonus — reward known-good release groups
    if use_trash
        && !rejected
        && let Some(filters) = trash_filters
    {
        let v = filters.tier_bonus(parsed.release_group.as_deref());
        score += v;
        bd.trash_tier_bonus = v;
    }

    bd.total = score;
    bd.rejected = rejected;
    bd.rejected_reason = reject_reasons.first().map(|r| r.to_string());

    QualityScore {
        score,
        parsed,
        rejected,
        reject_reasons,
        breakdown: bd,
    }
}

/// Score release health using indexer-provided signals.
/// Returns a bonus/penalty to add to the quality score.
/// Positive = more trustworthy, negative = suspicious/fake.
pub fn score_release_health(release: &crate::watch::Release) -> f64 {
    let mut health = 0.0;

    // Seeder-based scoring (torrents)
    if let Some(seeders) = release.seeders {
        if seeders == 0 {
            // Dead torrent or possible fake
            health -= 0.4;
        } else if seeders >= 50 {
            health += 0.1;
        } else if seeders >= 5 {
            health += 0.05;
        }
    }

    // Grab-based scoring (usenet)
    if let Some(grabs) = release.grabs {
        let publish_date = time::OffsetDateTime::parse(
            &release.publish_date,
            &time::format_description::well_known::Rfc3339,
        )
        .ok();

        let is_old = publish_date
            .map(|d| {
                let age = time::OffsetDateTime::now_utc() - d;
                age.whole_hours() > 24
            })
            .unwrap_or(false);

        if grabs == 0 && is_old {
            health -= 0.2;
        } else if grabs >= 100 {
            health += 0.1;
        } else if grabs >= 10 {
            health += 0.05;
        }
    }

    // Size heuristics - suspiciously small files for claimed quality
    let title_lower = release.title.to_lowercase();
    let size_mb = release.size as f64 / (1024.0 * 1024.0);

    if (title_lower.contains("1080p") || title_lower.contains("bluray")) && size_mb < 300.0 {
        health -= 0.5; // Likely fake
    } else if title_lower.contains("2160p") && size_mb < 1000.0 {
        health -= 0.5; // Likely fake 4K
    } else if title_lower.contains("720p") && size_mb < 100.0 {
        health -= 0.3; // Suspicious
    }

    // Age-based scoring
    if let Ok(publish_date) = time::OffsetDateTime::parse(
        &release.publish_date,
        &time::format_description::well_known::Rfc3339,
    ) {
        let age = time::OffsetDateTime::now_utc() - publish_date;
        let age_days = age.whole_days();

        let has_activity = release.seeders.unwrap_or(0) > 0 || release.grabs.unwrap_or(0) > 0;

        if age_days > 7 && !has_activity {
            health -= 0.3; // Old release with no activity
        }
    }

    health
}

pub fn merge_preferences(
    profile: Option<&QualityProfileRow>,
    overrides: Option<&QualityOverrides>,
) -> QualityPreferences {
    let mut prefs = QualityPreferences {
        min_resolution: profile.and_then(|p| p.min_resolution.clone()),
        max_resolution: profile.and_then(|p| p.max_resolution.clone()),
        preferred_sources: profile
            .map(|p| p.preferred_sources.clone())
            .unwrap_or_default(),
        preferred_codecs: profile
            .map(|p| p.preferred_codecs.clone())
            .unwrap_or_default(),
        preferred_audio: profile
            .map(|p| p.preferred_audio.clone())
            .unwrap_or_default(),
        preferred_languages: profile
            .map(|p| p.preferred_languages.clone())
            .unwrap_or_default(),
        rejected_sources: profile
            .map(|p| p.rejected_sources.clone())
            .unwrap_or_default(),
        rejected_languages: profile
            .map(|p| p.rejected_languages.clone())
            .unwrap_or_default(),
        rejected_terms: profile
            .map(|p| p.rejected_terms.clone())
            .unwrap_or_default(),
        reject_hardcoded_subs: profile.map(|p| p.reject_hardcoded_subs).unwrap_or(false),
        reject_remux: profile.map(|p| p.reject_remux).unwrap_or(false),
    };

    if let Some(overrides) = overrides {
        if let Some(min) = overrides.min_resolution.as_ref() {
            prefs.min_resolution = Some(min.clone());
        }
        if let Some(max) = overrides.max_resolution.as_ref() {
            prefs.max_resolution = Some(max.clone());
        }
        if let Some(source) = overrides.source.as_ref() {
            prefs.preferred_sources = vec![source.clone()];
        }
        if let Some(codec) = overrides.codec.as_ref() {
            prefs.preferred_codecs = vec![codec.clone()];
        }
        if let Some(audio) = overrides.audio.as_ref() {
            prefs.preferred_audio = vec![audio.clone()];
        }
        if let Some(langs) = overrides.preferred_languages.as_ref() {
            prefs.preferred_languages = langs.clone();
        }
        if let Some(langs) = overrides.rejected_languages.as_ref() {
            prefs.rejected_languages = langs.clone();
        }
    }

    prefs
}

// =============================================================================
// Parsing Functions
// =============================================================================

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
    let source = detect_source(&lower);

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
        || has_word(&lower, "chtsub"); // Chinese traditional subs

    // Advanced audio flags
    let is_atmos = has_word(&lower, "atmos") || has_word(&lower, "dolby.atmos");
    let is_dtsx =
        has_word(&lower, "dts-x") || has_word(&lower, "dtsx") || has_word(&lower, "dts:x");

    // Nuked/obfuscated
    let is_nuked = has_word(&lower, "nuked") || has_word(&lower, "nuke");
    let is_obfuscated = OBFUSCATED_RE.is_match(&lower);

    // Language
    let language = LANGUAGES
        .iter()
        .find(|(word, _)| has_word(&lower, word))
        .map(|(_, normalized)| normalized.to_string());

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

/// Map video height (pixels) to a resolution tier for upgrade comparison.
///
/// Higher tier = better. Only tier jumps count as "clear upgrades" in the
/// library-aware fallback path (when Kobload has no download history but
/// Kobbler reports existing quality).
pub fn resolution_tier(height: i32) -> u8 {
    match height {
        h if h >= 2160 => 3, // 4K / UHD
        h if h >= 1080 => 2, // 1080p
        h if h >= 720 => 1,  // 720p
        _ => 0,              // SD
    }
}

/// Whether a file already held in the library is good enough that a profile
/// would not want it replaced — i.e. its resolution tier is at or above the
/// profile's `max_resolution` target.
///
/// "In library" alone is NOT sufficient to call a want satisfied: a 720p file
/// against a 1080p profile is still an outstanding upgrade. This answers the
/// narrower question "is the held file at target?".
///
/// Returns `false` when the height is unknown (unprobed) or the profile has no
/// `max_resolution` — in both cases we cannot prove the file is at target, and
/// the conservative answer is to keep looking. `max_resolution` doubles as the
/// target tier here, matching `should_wait_for_target_tier`'s existing use.
pub fn library_meets_profile_target(
    library_height: Option<i32>,
    max_resolution: Option<&str>,
) -> bool {
    let Some(height) = library_height else {
        return false;
    };
    let Some(target_tier) = max_resolution
        .and_then(parse_resolution)
        .map(resolution_tier)
    else {
        return false;
    };
    resolution_tier(height) >= target_tier
}

/// Map a canonical source string (as returned by source parsing) to an ordered
/// quality tier (higher = better source). Used by the upgrade gate so a genuine
/// source jump at the same resolution (HDTV → WEB) counts as an upgrade, while
/// churn within a tier (WEB-DL vs WEBRIP — "any WEB is acceptable") does not.
///
/// Tiers:
/// - 5 disc:     remux, bluray, hddvd
/// - 4 web:      web-dl, webrip, web, vod (all "WEB" — treated equal)
/// - 3 hd-bcast: hdtv, pdtv
/// - 2 dvd:      dvd, dvdrip, dvdr
/// - 1 sd-bcast: sdtv, dsr, satellite, ppv
/// - 0 pre-rel:  cam, telesync, telecine, screener, dvdscr, bdscr, r5, workprint
///
/// Unknown sources return `None` — the caller must not treat that as an upgrade.
pub fn source_tier(source: &str) -> Option<u8> {
    let s = source.trim().to_ascii_lowercase();
    let tier = match s.as_str() {
        "remux" | "bluray" | "hddvd" => 5,
        "web-dl" | "webdl" | "webrip" | "web" | "vod" => 4,
        "hdtv" | "pdtv" => 3,
        "dvd" | "dvdrip" | "dvdr" => 2,
        "sdtv" | "dsr" | "satellite" | "ppv" => 1,
        "cam" | "telesync" | "telecine" | "screener" | "dvdscr" | "bdscr" | "r5" | "workprint" => 0,
        _ => return None,
    };
    Some(tier)
}

/// Parse resolution string to numeric value.
pub fn parse_resolution(value: &str) -> Option<i32> {
    let normalized = value.trim().to_lowercase();

    // Check named resolutions first
    for (aliases, _, height) in RESOLUTIONS {
        for alias in *aliases {
            if normalized == *alias || normalized == format!("{}p", alias) {
                return Some(*height);
            }
        }
    }

    // Try parsing numeric value
    if normalized == "4k" || normalized == "uhd" {
        return Some(2160);
    }
    if normalized == "8k" {
        return Some(4320);
    }

    let digits: String = normalized.chars().filter(|c| c.is_ascii_digit()).collect();
    digits.parse::<i32>().ok()
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_quality_basic() {
        let parsed = parse_quality("Movie.2024.1080p.WEB-DL.x265.DTS");
        assert_eq!(parsed.resolution.as_deref(), Some("1080p"));
        assert_eq!(parsed.source.as_deref(), Some("web-dl"));
        assert_eq!(parsed.codec.as_deref(), Some("hevc"));
        assert_eq!(parsed.audio.as_deref(), Some("dts"));
    }

    #[test]
    fn test_parse_quality_bracket_source() {
        // [PDTV-drngr] — PDTV is a source inside brackets, should be parsed
        let parsed = parse_quality("Law & Order - 8x12 - Expert [PDTV-drngr]");
        assert_eq!(
            parsed.source.as_deref(),
            Some("pdtv"),
            "PDTV inside brackets should be parsed as source"
        );
    }

    #[test]
    fn test_ts_no_false_positive() {
        // "Monsters" should NOT match TS source
        let parsed = parse_quality("Monsters.2024.1080p.WEB-DL");
        assert_ne!(parsed.source.as_deref(), Some("telesync"));

        // "Knights" should NOT match TS source
        let parsed = parse_quality("Knights.Of.The.Round.Table.720p");
        assert_ne!(parsed.source.as_deref(), Some("telesync"));

        // Actual TS should match
        let parsed = parse_quality("Movie.2024.TS.x264");
        assert_eq!(parsed.source.as_deref(), Some("telesync"));

        let parsed = parse_quality("Movie.2024.HDTS.x264");
        assert_eq!(parsed.source.as_deref(), Some("telesync"));

        let parsed = parse_quality("Movie.2024.TELESYNC.x264");
        assert_eq!(parsed.source.as_deref(), Some("telesync"));

        // Underscore-separated titles (usenet) must parse the same way
        let parsed = parse_quality("Resident_Evil_2026_1080p_TeleSync_AAC_AV1-LuCY");
        assert_eq!(parsed.source.as_deref(), Some("telesync"));
        assert_eq!(parsed.resolution.as_deref(), Some("1080p"));
        assert_eq!(parsed.audio.as_deref(), Some("aac"));
        assert_eq!(parsed.release_group.as_deref(), Some("LuCY"));
    }

    #[test]
    fn test_proper_repack_remux() {
        let parsed = parse_quality("Movie.2024.1080p.PROPER.WEB-DL.x265");
        assert!(parsed.is_proper);

        let parsed = parse_quality("Movie.2024.1080p.REPACK.BluRay.x265");
        assert!(parsed.is_repack);

        let parsed = parse_quality("Movie.2024.1080p.REMUX.BluRay");
        assert!(parsed.is_remux);
        assert_eq!(parsed.source.as_deref(), Some("remux"));
    }

    #[test]
    fn test_hardcoded_subs() {
        let parsed = parse_quality("Movie.2024.1080p.HC.WEB-DL");
        assert!(parsed.is_hardcoded_subs);

        let parsed = parse_quality("Movie.2024.HARDCODED.720p");
        assert!(parsed.is_hardcoded_subs);

        let parsed = parse_quality("Movie.2024.KORSUB.720p");
        assert!(parsed.is_hardcoded_subs);
    }

    #[test]
    fn test_language_parsing() {
        let parsed = parse_quality("Movie.2024.MULTI.1080p.BluRay");
        assert_eq!(parsed.language.as_deref(), Some("multi"));

        let parsed = parse_quality("Movie.2024.FRENCH.720p.WEB-DL");
        assert_eq!(parsed.language.as_deref(), Some("french"));
    }

    #[test]
    fn test_release_group() {
        let parsed = parse_quality("Movie.2024.1080p.BluRay.x265-SPARKS");
        assert_eq!(parsed.release_group.as_deref(), Some("SPARKS"));
    }

    #[test]
    fn test_cam_detection() {
        let parsed = parse_quality("Movie.2024.CAM.x264");
        assert_eq!(parsed.source.as_deref(), Some("cam"));

        let parsed = parse_quality("Movie.2024.HDCAM.x264");
        assert_eq!(parsed.source.as_deref(), Some("cam"));
    }

    #[test]
    fn test_rejected_source_scoring() {
        use time::OffsetDateTime;

        let profile = QualityProfileRow {
            id: uuid::Uuid::nil(),
            name: "TV - Any".to_string(),
            media_kind: "tv".to_string(),
            min_resolution: None,
            max_resolution: None,
            preferred_sources: Vec::new(),
            preferred_codecs: Vec::new(),
            preferred_audio: Vec::new(),
            preferred_languages: Vec::new(),
            rejected_sources: vec![
                "cam".to_string(),
                "telesync".to_string(),
                "telecine".to_string(),
            ],
            rejected_languages: Vec::new(),
            rejected_terms: Vec::new(),
            reject_hardcoded_subs: true,
            use_trash_filters: true,
            reject_remux: false,
            is_default: false,
            upgrade_cutoff_score: None,
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
        };

        // CAM release with rejected sources should score very low
        let score_cam = score_quality("Movie.2024.CAM.x264", Some(&profile), None, None);
        assert!(
            score_cam.score < -0.5,
            "CAM should be heavily penalized: {}",
            score_cam.score
        );

        // Same release without rejected sources should score normally
        let no_reject_profile = QualityProfileRow {
            rejected_sources: Vec::new(),
            ..profile.clone()
        };
        let score_no_reject =
            score_quality("Movie.2024.CAM.x264", Some(&no_reject_profile), None, None);
        assert!(
            score_no_reject.score > score_cam.score,
            "Without rejected sources, CAM should score higher"
        );

        // "ts" in rejected_sources must match parsed source "telesync"
        let ts_profile = QualityProfileRow {
            rejected_sources: vec!["ts".to_string()],
            ..profile.clone()
        };
        let score_ts = score_quality(
            "Scream.7.2026.1080p.TeleSync.AAC2.0.AV1-LuCY",
            Some(&ts_profile),
            None,
            None,
        );
        assert!(
            score_ts.rejected,
            "TeleSync should be rejected when 'ts' is in rejected_sources"
        );
        assert!(
            score_ts.score < -0.5,
            "TeleSync should be heavily penalized: {}",
            score_ts.score
        );
    }

    #[test]
    fn test_profile_alias_matching() {
        // Source aliases
        assert!(profile_value_matches("ts", "telesync", SOURCE_ALIASES));
        assert!(profile_value_matches("hdts", "telesync", SOURCE_ALIASES));
        assert!(profile_value_matches("tc", "telecine", SOURCE_ALIASES));
        assert!(profile_value_matches(
            "telesync",
            "telesync",
            SOURCE_ALIASES
        ));
        assert!(!profile_value_matches("ts", "cam", SOURCE_ALIASES));

        // Codec aliases: "x265" in profile must match parsed "hevc"
        assert!(profile_value_matches("x265", "hevc", CODEC_ALIASES));
        assert!(profile_value_matches("h265", "hevc", CODEC_ALIASES));
        assert!(profile_value_matches("x264", "avc", CODEC_ALIASES));
        assert!(profile_value_matches("hevc", "hevc", CODEC_ALIASES));
        assert!(!profile_value_matches("x265", "avc", CODEC_ALIASES));

        // Audio aliases
        assert!(profile_value_matches("dd+", "ddp", AUDIO_ALIASES));
        assert!(profile_value_matches("eac3", "ddp", AUDIO_ALIASES));
        assert!(profile_value_matches("ac3", "dd", AUDIO_ALIASES));
        assert!(profile_value_matches("ddp", "ddp", AUDIO_ALIASES));
    }

    #[test]
    fn test_codec_preference_with_aliases() {
        use time::OffsetDateTime;
        let profile = QualityProfileRow {
            id: uuid::Uuid::nil(),
            name: "test".to_string(),
            media_kind: "movie".to_string(),
            min_resolution: None,
            max_resolution: None,
            preferred_sources: Vec::new(),
            preferred_codecs: vec!["x265".to_string()],
            preferred_audio: Vec::new(),
            preferred_languages: Vec::new(),
            rejected_sources: Vec::new(),
            rejected_languages: Vec::new(),
            rejected_terms: Vec::new(),
            reject_hardcoded_subs: false,
            use_trash_filters: true,
            reject_remux: false,
            is_default: false,
            upgrade_cutoff_score: None,
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
        };

        // x265 in profile should give codec bonus for HEVC release
        let with_hevc = score_quality("Movie.2024.1080p.WEB-DL.x265", Some(&profile), None, None);
        let without_codec = score_quality("Movie.2024.1080p.WEB-DL", Some(&profile), None, None);
        assert!(
            with_hevc.score > without_codec.score,
            "HEVC release should get codec bonus with x265 preference: {} vs {}",
            with_hevc.score,
            without_codec.score
        );
    }

    #[test]
    fn test_ddp_eac3_detection() {
        let parsed = parse_quality("Movie.2024.1080p.WEB-DL.EAC3");
        assert_eq!(parsed.audio.as_deref(), Some("ddp"));

        let parsed = parse_quality("Movie.2024.1080p.WEB-DL.DD+5.1");
        assert_eq!(parsed.audio.as_deref(), Some("ddp"));
    }

    #[test]
    fn test_streaming_services() {
        let parsed = parse_quality("Show.S01E01.1080p.AMZN.WEB-DL");
        assert_eq!(parsed.streaming_service.as_deref(), Some("amazon"));

        let parsed = parse_quality("Show.S01E01.1080p.NF.WEB-DL");
        assert_eq!(parsed.streaming_service.as_deref(), Some("netflix"));

        let parsed = parse_quality("Show.S01E01.1080p.DSNP.WEB-DL");
        assert_eq!(parsed.streaming_service.as_deref(), Some("disney+"));

        let parsed = parse_quality("Show.S01E01.2160p.ATVP.WEB-DL");
        assert_eq!(parsed.streaming_service.as_deref(), Some("appletv"));

        let parsed = parse_quality("Show.S01E01.1080p.HMAX.WEB-DL");
        assert_eq!(parsed.streaming_service.as_deref(), Some("hbomax"));
    }

    #[test]
    fn test_hdr_formats() {
        let parsed = parse_quality("Movie.2024.2160p.DV.WEB-DL");
        assert!(parsed.is_dolby_vision);
        assert!(parsed.is_hdr);

        let parsed = parse_quality("Movie.2024.2160p.DoVi.WEB-DL");
        assert!(parsed.is_dolby_vision);

        let parsed = parse_quality("Movie.2024.2160p.HDR10Plus.BluRay");
        assert!(parsed.is_hdr10plus);
        assert!(parsed.is_hdr);

        let parsed = parse_quality("Movie.2024.2160p.HDR10.WEB-DL");
        assert!(parsed.is_hdr10);
        assert!(parsed.is_hdr);
        assert!(!parsed.is_hdr10plus);

        let parsed = parse_quality("Movie.2024.2160p.HDR.WEB-DL");
        assert!(parsed.is_hdr);
    }

    #[test]
    fn test_language_iso_codes() {
        // ISO 639-2/B abbreviations
        let parsed = parse_quality("Movie.2024.ITA.1080p.BluRay");
        assert_eq!(parsed.language.as_deref(), Some("italian"));

        let parsed = parse_quality("Movie.2024.GER.720p.WEB-DL");
        assert_eq!(parsed.language.as_deref(), Some("german"));

        let parsed = parse_quality("Movie.2024.SPA.1080p.BluRay");
        assert_eq!(parsed.language.as_deref(), Some("spanish"));

        let parsed = parse_quality("Movie.2024.JPN.1080p.BluRay");
        assert_eq!(parsed.language.as_deref(), Some("japanese"));

        let parsed = parse_quality("Movie.2024.KOR.1080p.WEB-DL");
        assert_eq!(parsed.language.as_deref(), Some("korean"));

        let parsed = parse_quality("Movie.2024.FRE.720p.BluRay");
        assert_eq!(parsed.language.as_deref(), Some("french"));
    }

    #[test]
    fn test_language_scene_tags() {
        let parsed = parse_quality("Movie.2024.TRUEFRENCH.1080p.BluRay");
        assert_eq!(parsed.language.as_deref(), Some("french"));

        let parsed = parse_quality("Movie.2024.VOSTFR.720p.WEB-DL");
        assert_eq!(parsed.language.as_deref(), Some("french"));
    }

    #[test]
    fn test_itunes_not_italian() {
        // "iT" = iTunes streaming service, NOT Italian
        let parsed = parse_quality(
            "Teen.Titans.Go!.S09E36.Use.It.or.Lose.It.REPACK.1080p.iT.WEB-DL.AAC2.0.H.264-NTb",
        );
        assert_eq!(parsed.streaming_service.as_deref(), Some("itunes"));
        assert!(
            parsed.language.is_none(),
            "iT should not be detected as Italian, got: {:?}",
            parsed.language
        );
    }

    #[test]
    fn test_no_language_detected() {
        // Standard English releases typically have no language tag
        let parsed = parse_quality("Movie.2024.1080p.BluRay.x265-GROUP");
        assert!(parsed.language.is_none());
    }

    fn make_english_profile() -> QualityProfileRow {
        use time::OffsetDateTime;
        QualityProfileRow {
            id: uuid::Uuid::nil(),
            name: "Test".to_string(),
            media_kind: "tv".to_string(),
            min_resolution: None,
            max_resolution: None,
            preferred_sources: Vec::new(),
            preferred_codecs: Vec::new(),
            preferred_audio: Vec::new(),
            preferred_languages: vec!["english".to_string()],
            rejected_languages: Vec::new(),
            rejected_terms: Vec::new(),
            rejected_sources: Vec::new(),
            reject_hardcoded_subs: false,
            use_trash_filters: true,
            reject_remux: false,
            is_default: false,
            upgrade_cutoff_score: None,
            created_at: OffsetDateTime::UNIX_EPOCH,
            updated_at: OffsetDateTime::UNIX_EPOCH,
        }
    }

    #[test]
    fn test_language_hard_reject_non_preferred() {
        let profile = make_english_profile();

        // Italian release should be hard-rejected when English is preferred
        let result = score_quality("Movie.2024.ITA.1080p.BluRay", Some(&profile), None, None);
        assert!(
            result.rejected,
            "Italian release should be rejected with English-only profile"
        );

        // French release should also be rejected
        let result = score_quality("Movie.2024.FRENCH.720p.WEB-DL", Some(&profile), None, None);
        assert!(
            result.rejected,
            "French release should be rejected with English-only profile"
        );
    }

    #[test]
    fn test_no_language_passes_english_profile() {
        let profile = make_english_profile();

        // Release with no language tag should pass (English releases aren't tagged)
        let result = score_quality("Movie.2024.1080p.BluRay.x265", Some(&profile), None, None);
        assert!(
            !result.rejected,
            "No-language release should pass English profile"
        );
    }

    #[test]
    fn test_preferred_language_scores_higher() {
        let profile = make_english_profile();

        // English-tagged release should get a bonus
        let result = score_quality(
            "Movie.2024.ENGLISH.1080p.BluRay",
            Some(&profile),
            None,
            None,
        );
        assert!(!result.rejected);
        assert!(
            result.score > 0.0,
            "English release should score positively"
        );
    }

    #[test]
    fn test_multi_dual_passes_preferred_language() {
        let profile = make_english_profile();

        // MULTI releases include multiple languages (typically including English)
        // and should not be rejected by an English-preferred profile
        let multi = score_quality(
            "Movie.2024.MULTi.1080p.BluRay.x264",
            Some(&profile),
            None,
            None,
        );
        assert!(
            !multi.rejected,
            "MULTI release should pass English-preferred profile"
        );

        // DUAL similarly includes English
        let dual = score_quality("Movie.2024.DUAL.720p.WEB-DL", Some(&profile), None, None);
        assert!(
            !dual.rejected,
            "DUAL release should pass English-preferred profile"
        );

        // Multi should score lower than a confirmed English release
        let english = score_quality(
            "Movie.2024.ENGLISH.1080p.BluRay.x264",
            Some(&profile),
            None,
            None,
        );
        assert!(
            english.score > multi.score,
            "Confirmed English ({}) should rank above MULTI ({})",
            english.score,
            multi.score,
        );
    }

    #[test]
    fn test_rejected_languages() {
        let mut profile = make_english_profile();
        profile.rejected_languages = vec!["french".to_string()];

        // French should be rejected via rejected_languages even without preferred_languages check
        let result = score_quality("Movie.2024.FRENCH.1080p.BluRay", Some(&profile), None, None);
        assert!(
            result.rejected,
            "French should be rejected via rejected_languages"
        );
    }

    #[test]
    fn test_quality_override_disables_language_filter() {
        let profile = make_english_profile();

        // Override with empty preferred_languages = accept any language
        let overrides = QualityOverrides {
            preferred_languages: Some(Vec::new()),
            ..Default::default()
        };

        // Italian release should pass when override disables language filtering
        let result = score_quality(
            "Movie.2024.ITA.1080p.BluRay",
            Some(&profile),
            Some(&overrides),
            None,
        );
        assert!(
            !result.rejected,
            "Italian release should pass with empty language override"
        );
    }

    #[test]
    fn test_quality_override_specific_language() {
        let profile = make_english_profile();

        // Override to accept Japanese
        let overrides = QualityOverrides {
            preferred_languages: Some(vec!["japanese".to_string()]),
            ..Default::default()
        };

        // Japanese release should pass
        let result = score_quality(
            "Movie.2024.JPN.1080p.BluRay",
            Some(&profile),
            Some(&overrides),
            None,
        );
        assert!(
            !result.rejected,
            "Japanese release should pass with Japanese override"
        );

        // Italian should still be rejected (not in override list)
        let result = score_quality(
            "Movie.2024.ITA.1080p.BluRay",
            Some(&profile),
            Some(&overrides),
            None,
        );
        assert!(
            result.rejected,
            "Italian should be rejected with Japanese-only override"
        );
    }

    #[test]
    fn test_3d_formats() {
        let parsed = parse_quality("Movie.2024.1080p.3D.BluRay");
        assert!(parsed.is_3d);
        assert_eq!(parsed.format_3d.as_deref(), Some("3d"));

        let parsed = parse_quality("Movie.2024.1080p.HSBS.BluRay");
        assert!(parsed.is_3d);
        assert_eq!(parsed.format_3d.as_deref(), Some("hsbs"));

        let parsed = parse_quality("Movie.2024.1080p.MVC.BluRay");
        assert!(parsed.is_3d);
        assert_eq!(parsed.format_3d.as_deref(), Some("mvc"));
    }

    #[test]
    fn test_atmos_dtsx() {
        let parsed = parse_quality("Movie.2024.2160p.TrueHD.Atmos.BluRay");
        assert!(parsed.is_atmos);
        assert_eq!(parsed.audio.as_deref(), Some("atmos"));

        let parsed = parse_quality("Movie.2024.2160p.DTS-X.BluRay");
        assert!(parsed.is_dtsx);
        assert_eq!(parsed.audio.as_deref(), Some("dts-x"));
    }

    #[test]
    fn test_release_flags() {
        let parsed = parse_quality("Movie.2024.1080p.INTERNAL.BluRay");
        assert!(parsed.is_internal);

        let parsed = parse_quality("Movie.2024.1080p.RETAIL.BluRay");
        assert!(parsed.is_retail);

        let parsed = parse_quality("Movie.2024.1080p.LIMITED.BluRay");
        assert!(parsed.is_limited);

        let parsed = parse_quality("Movie.2024.1080p.REAL.PROPER.BluRay");
        assert!(parsed.is_real);
        assert!(parsed.is_proper);
    }

    #[test]
    fn test_fix_flags() {
        let parsed = parse_quality("Movie.2024.1080p.NFOFIX.BluRay-GROUP");
        assert!(parsed.is_nfo_fix);

        let parsed = parse_quality("Movie.2024.1080p.SAMPLEFIX.BluRay-GROUP");
        assert!(parsed.is_sample_fix);

        let parsed = parse_quality("Movie.2024.1080p.SYNCFIX.BluRay-GROUP");
        assert!(parsed.is_sync_fix);

        let parsed = parse_quality("Movie.2024.1080p.DIRFIX.BluRay-GROUP");
        assert!(parsed.is_dirfix);
    }

    #[test]
    fn test_version() {
        let parsed = parse_quality("Movie.2024.1080p.v2.BluRay");
        assert_eq!(parsed.version, Some(2));

        let parsed = parse_quality("Movie.2024.1080p.VERSION2.BluRay");
        assert_eq!(parsed.version, Some(2));
    }

    #[test]
    fn test_pdtv_source() {
        let parsed = parse_quality("Show.S01E01.PDTV.x264");
        assert_eq!(parsed.source.as_deref(), Some("pdtv"));
    }

    #[test]
    fn test_r5_source() {
        let parsed = parse_quality("Movie.2024.R5.LINE.DUBBED.x264");
        assert_eq!(parsed.source.as_deref(), Some("r5"));
    }

    #[test]
    fn test_satellite_source() {
        let parsed = parse_quality("Show.S01E01.SatRip.x264");
        assert_eq!(parsed.source.as_deref(), Some("satellite"));
    }

    #[test]
    fn test_hevc_aliases() {
        let parsed = parse_quality("Movie.2024.1080p.HEVC.WEB-DL");
        assert_eq!(parsed.codec.as_deref(), Some("hevc"));

        let parsed = parse_quality("Movie.2024.1080p.H265.WEB-DL");
        assert_eq!(parsed.codec.as_deref(), Some("hevc"));

        let parsed = parse_quality("Movie.2024.1080p.x265.WEB-DL");
        assert_eq!(parsed.codec.as_deref(), Some("hevc"));
    }

    #[test]
    fn test_av1_codec() {
        let parsed = parse_quality("Movie.2024.1080p.AV1.WEB-DL");
        assert_eq!(parsed.codec.as_deref(), Some("av1"));
    }

    #[test]
    fn test_truehd_audio() {
        let parsed = parse_quality("Movie.2024.1080p.TrueHD.BluRay");
        assert_eq!(parsed.audio.as_deref(), Some("truehd"));
    }

    #[test]
    fn test_dts_hd_ma() {
        let parsed = parse_quality("Movie.2024.1080p.DTS-HD.MA.BluRay");
        assert_eq!(parsed.audio.as_deref(), Some("dts-hd.ma"));
    }

    #[test]
    fn test_resolution_parsing() {
        assert_eq!(parse_resolution("4k"), Some(2160));
        assert_eq!(parse_resolution("2160p"), Some(2160));
        assert_eq!(parse_resolution("1080p"), Some(1080));
        assert_eq!(parse_resolution("720p"), Some(720));
        assert_eq!(parse_resolution("480p"), Some(480));
        assert_eq!(parse_resolution("uhd"), Some(2160));
    }

    #[test]
    fn test_hdr_scoring_bonus() {
        let parsed_dv = parse_quality("Movie.2024.2160p.DV.WEB-DL");
        let parsed_hdr10plus = parse_quality("Movie.2024.2160p.HDR10Plus.WEB-DL");
        let parsed_hdr10 = parse_quality("Movie.2024.2160p.HDR10.WEB-DL");
        let parsed_sdr = parse_quality("Movie.2024.2160p.WEB-DL");

        let score_dv = score_quality("Movie.2024.2160p.DV.WEB-DL", None, None, None);
        let score_hdr10plus = score_quality("Movie.2024.2160p.HDR10Plus.WEB-DL", None, None, None);
        let score_hdr10 = score_quality("Movie.2024.2160p.HDR10.WEB-DL", None, None, None);
        let score_sdr = score_quality("Movie.2024.2160p.WEB-DL", None, None, None);

        assert!(
            score_dv.score > score_hdr10plus.score,
            "DV should score higher than HDR10+"
        );
        assert!(
            score_hdr10plus.score > score_hdr10.score,
            "HDR10+ should score higher than HDR10"
        );
        assert!(
            score_hdr10.score > score_sdr.score,
            "HDR10 should score higher than SDR"
        );

        // Verify the flags are set correctly
        assert!(parsed_dv.is_dolby_vision);
        assert!(parsed_hdr10plus.is_hdr10plus);
        assert!(parsed_hdr10.is_hdr10);
        assert!(!parsed_sdr.is_hdr);
    }

    #[test]
    fn test_nuked_penalty() {
        let score_nuked = score_quality("Movie.2024.1080p.NUKED.WEB-DL", None, None, None);
        let score_normal = score_quality("Movie.2024.1080p.WEB-DL", None, None, None);

        assert!(
            score_nuked.score < score_normal.score - 0.4,
            "Nuked should be heavily penalized"
        );
    }

    #[test]
    fn test_crunchyroll_funimation() {
        let parsed = parse_quality("[SubGroup] Anime - 01 [1080p] [CR].mkv");
        assert_eq!(parsed.streaming_service.as_deref(), Some("crunchyroll"));

        let parsed = parse_quality("[SubGroup] Anime - 01 [1080p] [Funimation].mkv");
        assert_eq!(parsed.streaming_service.as_deref(), Some("funimation"));
    }

    #[test]
    fn test_resolution_tier_bonus() {
        // Same release at different resolutions should score differently
        let s720 = score_quality("Movie.2024.720p.WEB-DL.x264.DDP", None, None, None);
        let s1080 = score_quality("Movie.2024.1080p.WEB-DL.x264.DDP", None, None, None);
        let s2160 = score_quality("Movie.2024.2160p.WEB-DL.x264.DDP", None, None, None);

        assert!(
            s1080.score > s720.score,
            "1080p ({}) should score higher than 720p ({})",
            s1080.score,
            s720.score
        );
        assert!(
            s2160.score > s1080.score,
            "2160p ({}) should score higher than 1080p ({})",
            s2160.score,
            s1080.score
        );

        // Verify the tier gaps are ~0.15 each (larger than default 0.10 margin)
        let gap_720_1080 = s1080.score - s720.score;
        let gap_1080_2160 = s2160.score - s1080.score;
        assert!(
            (gap_720_1080 - 0.15).abs() < 0.01,
            "720p→1080p gap should be ~0.15, got {}",
            gap_720_1080
        );
        assert!(
            (gap_1080_2160 - 0.15).abs() < 0.01,
            "1080p→2160p gap should be ~0.15, got {}",
            gap_1080_2160
        );
    }

    /// Regression: a TRaSH-tiered 720p release must not outrank an untiered
    /// 1080p one.
    ///
    /// Real case (2026-07-26): "Primer.2004.720p.BluRay.AAC2.0.x264-ZQ" was
    /// grabbed over ~100 1080p candidates. Group ZQ is hd_bluray_tier_01, and
    /// the tier bonus was 0.30 — double the 0.15 gap between the 720p and 1080p
    /// resolution tiers — so group reputation outweighed resolution entirely.
    /// Tier bonuses are now capped at 0.10 (tier-01), strictly below that gap,
    /// so a tier bonus can break ties within a resolution but never buy a
    /// downgrade across one.
    #[test]
    fn test_tier_bonus_cannot_outrank_higher_resolution() {
        let profile = QualityProfileRow {
            max_resolution: Some("1080p".to_string()),
            preferred_sources: vec![
                "bluray".to_string(),
                "web-dl".to_string(),
                "webrip".to_string(),
            ],
            preferred_codecs: vec!["x265".to_string(), "x264".to_string()],
            preferred_audio: vec!["dts".to_string(), "ddp".to_string()],
            media_kind: "movie".to_string(),
            reject_remux: true,
            ..make_english_profile()
        };

        // ZQ is a tier-01 group; nikt0 is untiered — as in the real TRaSH data.
        let filters = crate::trash_filters::TrashFilterSet::from_tier_bonuses(&[("zq", 0.10)]);

        let s720 = score_quality(
            "Primer.2004.720p.BluRay.AAC2.0.x264-ZQ",
            Some(&profile),
            None,
            Some(&filters),
        );
        let s1080 = score_quality(
            "Primer.2004.1080p.BluRay.x264-nikt0",
            Some(&profile),
            None,
            Some(&filters),
        );

        assert!(
            s1080.score > s720.score,
            "untiered 1080p ({}) must outrank tier-01 720p ({})",
            s1080.score,
            s720.score
        );

        // Same resolution: the tier bonus should still break the tie.
        let s1080_tiered = score_quality(
            "Primer.2004.1080p.BluRay.x264-ZQ",
            Some(&profile),
            None,
            Some(&filters),
        );
        assert!(
            s1080_tiered.score > s1080.score,
            "within a resolution, tiered ({}) should beat untiered ({})",
            s1080_tiered.score,
            s1080.score
        );
    }

    #[test]
    fn test_resolution_tier_bonus_no_resolution() {
        // No resolution → no tier bonus
        let with_res = score_quality("Movie.2024.1080p.WEB-DL", None, None, None);
        let without_res = score_quality("Movie.2024.WEB-DL", None, None, None);

        assert!(
            with_res.score > without_res.score,
            "Release with resolution ({}) should score higher than without ({})",
            with_res.score,
            without_res.score
        );
    }

    #[test]
    fn test_rejected_terms() {
        let mut profile = make_english_profile();
        profile.rejected_terms = vec![
            "ASL".to_string(),
            "BSL".to_string(),
            ".Signed.".to_string(),
            "Sign.Zone".to_string(),
            "SignZone".to_string(),
        ];

        // ASL in title should reject
        let result = score_quality("Show.S01E01.ASL.1080p.WEB-DL", Some(&profile), None, None);
        assert!(result.rejected, "ASL release should be rejected");
        assert_eq!(
            result.primary_reject_reason(),
            Some(nats_jobs::QualityRejectReason::RejectedTerm)
        );

        // Case-insensitive: lowercase asl should also reject
        let result = score_quality("Show.S01E01.asl.1080p.WEB-DL", Some(&profile), None, None);
        assert!(result.rejected, "lowercase asl should be rejected");

        // BSL should reject
        let result = score_quality("Show.S01E01.BSL.720p.HDTV", Some(&profile), None, None);
        assert!(result.rejected, "BSL release should be rejected");

        // .Signed. with dots should reject
        let result = score_quality(
            "Show.S01E01.Signed.1080p.WEB-DL",
            Some(&profile),
            None,
            None,
        );
        assert!(result.rejected, ".Signed. release should be rejected");

        // Sign.Zone should reject
        let result = score_quality(
            "Show.S01E01.Sign.Zone.1080p.HDTV",
            Some(&profile),
            None,
            None,
        );
        assert!(result.rejected, "Sign.Zone release should be rejected");

        // SignZone should reject
        let result = score_quality(
            "Show.S01E01.SignZone.1080p.HDTV",
            Some(&profile),
            None,
            None,
        );
        assert!(result.rejected, "SignZone release should be rejected");

        // Normal release should not be rejected
        let result = score_quality("Show.S01E01.1080p.WEB-DL.x265", Some(&profile), None, None);
        assert!(!result.rejected, "Normal release should not be rejected");

        // Empty rejected_terms should not reject anything
        profile.rejected_terms = Vec::new();
        let result = score_quality("Show.S01E01.ASL.1080p.WEB-DL", Some(&profile), None, None);
        assert!(
            result.primary_reject_reason() != Some(nats_jobs::QualityRejectReason::RejectedTerm),
            "Empty rejected_terms should not trigger rejection"
        );

        // Token boundary: "ASL" should NOT match inside "CASTLE"
        profile.rejected_terms = vec!["ASL".to_string()];
        let result = score_quality(
            "Show.S01E01.CASTLE.1080p.WEB-DL",
            Some(&profile),
            None,
            None,
        );
        assert!(
            !result.rejected,
            "ASL should not match inside CASTLE (token boundary)"
        );

        // Token boundary: "3D" should NOT match inside "X3D" or "3DO"
        profile.rejected_terms = vec!["3D".to_string()];
        let result = score_quality("Show.S01E01.X3D.1080p.WEB-DL", Some(&profile), None, None);
        assert!(!result.rejected, "3D should not match inside X3D");

        // Token boundary: "3D" SHOULD match when delimited
        let result = score_quality(
            "Movie.2024.3D.1080p.HSBS.BluRay",
            Some(&profile),
            None,
            None,
        );
        assert!(result.rejected, "3D should match when dot-delimited");

        // Token boundary: term at start/end of title
        let result = score_quality("3D.Movie.2024.1080p", Some(&profile), None, None);
        assert!(result.rejected, "3D at start of title should match");
        let result = score_quality("Movie.2024.1080p.3D", Some(&profile), None, None);
        assert!(result.rejected, "3D at end of title should match");

        // Delimiter with dots in term: ".Signed." should still work
        profile.rejected_terms = vec![".Signed.".to_string()];
        let result = score_quality(
            "Show.S01E01.Designed.1080p.WEB-DL",
            Some(&profile),
            None,
            None,
        );
        assert!(
            !result.rejected,
            ".Signed. should not match inside Designed"
        );
    }

    #[test]
    fn test_rejected_terms_separator_insensitive() {
        let mut profile = make_english_profile();
        profile.rejected_terms = vec!["Audio.Description".to_string()];

        // The real-world case: dot-separated, mid-title after "with".
        let result = score_quality(
            "Ted.Lasso.S04E04.Greyhounds.Day.Off.with.Audio.Description.1080p.ATVP.WEB-DL.DDP5.1.Atmos.H.264-Kitsune",
            Some(&profile),
            None,
            None,
        );
        assert!(
            result.rejected,
            "dot-separated Audio.Description should be rejected"
        );
        assert_eq!(
            result.primary_reject_reason(),
            Some(nats_jobs::QualityRejectReason::RejectedTerm)
        );

        // Every separator style the same term must now cover.
        for title in [
            "Ted Lasso S04E04 with Audio Description 1080p WEB-DL",
            "Ted_Lasso_S04E04_Audio_Description_1080p_WEB-DL",
            "Ted-Lasso-S04E04-Audio-Description-1080p-WEB-DL",
            "Ted.Lasso.S04E04.Audio_Description.1080p.WEB-DL",
        ] {
            let result = score_quality(title, Some(&profile), None, None);
            assert!(
                result.rejected,
                "should be rejected regardless of separator: {title}"
            );
        }

        // The clean release of the same episode must still pass.
        let result = score_quality(
            "Ted.Lasso.S04E04.Greyhounds.Day.Off.1080p.ATVP.WEB-DL.DDP5.1.Atmos.H.264-Kitsune",
            Some(&profile),
            None,
            None,
        );
        assert!(
            !result.rejected,
            "clean release without audio description should not be rejected"
        );

        // Token boundaries survive normalization — no substring false positives.
        let result = score_quality(
            "Show.S01E01.Audio.Descriptions.Behind.The.Scenes.1080p",
            Some(&profile),
            None,
            None,
        );
        assert!(
            !result.rejected,
            "should not match inside the longer word Descriptions"
        );

        // A term written without separators stays strict: it must not be
        // satisfied by a separated title.
        profile.rejected_terms = vec!["SignZone".to_string()];
        let result = score_quality(
            "Show.S01E01.Sign.Zone.1080p.HDTV",
            Some(&profile),
            None,
            None,
        );
        assert!(
            !result.rejected,
            "SignZone (unseparated term) should not match separated Sign.Zone"
        );
        let result = score_quality(
            "Show.S01E01.SignZone.1080p.HDTV",
            Some(&profile),
            None,
            None,
        );
        assert!(
            result.rejected,
            "SignZone should match the unseparated form"
        );
    }

    #[test]
    fn test_normalize_separators() {
        assert_eq!(
            normalize_separators(".audio..description."),
            "audio description"
        );
        assert_eq!(
            normalize_separators("_audio description_"),
            "audio description"
        );
        assert_eq!(
            normalize_separators("audio-description"),
            "audio description"
        );
        assert_eq!(normalize_separators("audiodescription"), "audiodescription");
        assert_eq!(normalize_separators(""), "");
        assert_eq!(normalize_separators("..."), "");
    }

    #[test]
    fn test_default_profile_rejects_audio_description() {
        // Guards the seeded preset list, not just a hand-built profile.
        let mut profile = make_english_profile();
        profile.rejected_terms = crate::startup::default_video_rejected_terms()
            .iter()
            .map(|s| s.to_string())
            .collect();

        let result = score_quality(
            "Ted.Lasso.S04E04.Greyhounds.Day.Off.with.Audio.Description.1080p.ATVP.WEB-DL.DDP5.1.Atmos.H.264-Kitsune",
            Some(&profile),
            None,
            None,
        );
        assert!(
            result.rejected,
            "default TV profile terms should reject an audio-described release"
        );

        let result = score_quality(
            "Ted.Lasso.S04E04.Greyhounds.Day.Off.1080p.ATVP.WEB-DL.DDP5.1.Atmos.H.264-Kitsune",
            Some(&profile),
            None,
            None,
        );
        assert!(
            !result.rejected,
            "default TV profile terms should accept the clean release"
        );
    }
}

#[cfg(test)]
mod health_tests {
    use super::*;
    use crate::watch::Release;

    fn make_release(title: &str) -> Release {
        Release {
            guid: String::new(),
            title: title.to_string(),
            file_name: None,
            download_url: None,
            magnet_url: None,
            info_url: None,
            size: 5_000_000_000, // 5 GB default (reasonable for HD content)
            publish_date: time::OffsetDateTime::now_utc()
                .format(&time::format_description::well_known::Rfc3339)
                .unwrap(),
            category_ids: Vec::new(),
            indexer: String::new(),
            protocol: "torrent".to_string(),
            info_hash: None,
            files: None,
            seeders: None,
            peers: None,
            leechers: None,
            grabs: None,
            votes: None,
            indexer_id: None,
            indexer_source_id: None,
            raw_data: serde_json::Value::Null,
            external_ids: Vec::new(),
            media: None,
        }
    }

    #[test]
    fn test_dead_torrent_penalty() {
        let mut r = make_release("Movie.2024.1080p.BluRay");
        r.seeders = Some(0);
        assert!(score_release_health(&r) < 0.0);
    }

    #[test]
    fn test_healthy_torrent_bonus() {
        let mut r = make_release("Movie.2024.1080p.BluRay");
        r.seeders = Some(100);
        assert!(score_release_health(&r) > 0.0);
    }

    #[test]
    fn test_suspicious_size() {
        let mut r = make_release("Movie.2024.1080p.BluRay.x264");
        r.size = 100 * 1024 * 1024; // 100MB for a 1080p BluRay
        assert!(score_release_health(&r) < -0.3);
    }

    #[test]
    fn test_resolution_tier_sd() {
        assert_eq!(resolution_tier(480), 0);
        assert_eq!(resolution_tier(360), 0);
        assert_eq!(resolution_tier(0), 0);
    }

    #[test]
    fn test_resolution_tier_720p() {
        assert_eq!(resolution_tier(720), 1);
        assert_eq!(resolution_tier(719), 0);
    }

    #[test]
    fn test_resolution_tier_1080p() {
        assert_eq!(resolution_tier(1080), 2);
        assert_eq!(resolution_tier(1079), 1);
    }

    #[test]
    fn test_resolution_tier_4k() {
        assert_eq!(resolution_tier(2160), 3);
        assert_eq!(resolution_tier(2159), 2);
        assert_eq!(resolution_tier(4320), 3);
    }

    #[test]
    fn test_library_meets_profile_target_at_or_above() {
        // At target and above both count as satisfied.
        assert!(library_meets_profile_target(Some(1080), Some("1080p")));
        assert!(library_meets_profile_target(Some(2160), Some("1080p")));
    }

    #[test]
    fn test_library_meets_profile_target_below_is_still_wanted() {
        // The case that matters: in-library but below target is NOT satisfied,
        // so a 720p holding still gets upgraded to the 1080p the profile asks for.
        assert!(!library_meets_profile_target(Some(720), Some("1080p")));
        assert!(!library_meets_profile_target(Some(480), Some("720p")));
    }

    #[test]
    fn test_library_meets_profile_target_unknown_is_conservative() {
        // Unprobed file or no profile ceiling — we can't prove it's at target,
        // so keep looking rather than silently declaring the want satisfied.
        assert!(!library_meets_profile_target(None, Some("1080p")));
        assert!(!library_meets_profile_target(Some(1080), None));
        assert!(!library_meets_profile_target(None, None));
    }

    #[test]
    fn test_source_tier_ordering() {
        // disc > web > hd-broadcast > dvd > sd-broadcast > pre-release
        assert!(source_tier("bluray") > source_tier("web-dl"));
        assert!(source_tier("web-dl") > source_tier("hdtv"));
        assert!(source_tier("hdtv") > source_tier("dvdrip"));
        assert!(source_tier("dvdrip") > source_tier("sdtv"));
        assert!(source_tier("sdtv") > source_tier("cam"));
        assert_eq!(source_tier("remux"), Some(5));
        assert_eq!(source_tier("cam"), Some(0));
    }

    #[test]
    fn test_source_tier_all_web_equal() {
        // "Any WEB is acceptable" — WEB-DL, WEBRIP, bare WEB and VOD share a tier.
        let web = source_tier("web-dl");
        assert_eq!(source_tier("webrip"), web);
        assert_eq!(source_tier("web"), web);
        assert_eq!(source_tier("vod"), web);
    }

    #[test]
    fn test_source_tier_unknown_is_none() {
        assert_eq!(source_tier("flibble"), None);
        assert_eq!(source_tier(""), None);
    }

    #[test]
    fn test_framerate_suffixed_resolution() {
        // 1080p50 — common in sports broadcasts (e.g., DornaRip MotoGP)
        let parsed = parse_quality("MotoGP 2026-Season Launch-1080p50 DornaRip HEVC AAC x265-deef");
        assert_eq!(parsed.resolution.as_deref(), Some("1080p"));

        // 720p50
        let parsed = parse_quality("Show.2024.720p50.HDTV.x264-GRP");
        assert_eq!(parsed.resolution.as_deref(), Some("720p"));

        // 1080p60
        let parsed = parse_quality("Event.1080p60.WEB-DL.H264");
        assert_eq!(parsed.resolution.as_deref(), Some("1080p"));

        // 576i25 — PAL interlaced
        let parsed = parse_quality("Show.576i25.DVB.MPEG2");
        assert_eq!(parsed.resolution.as_deref(), Some("576i"));

        // 2160p60 — 4K high framerate
        let parsed = parse_quality("Movie.2160p60.WEB-DL.HEVC");
        assert_eq!(parsed.resolution.as_deref(), Some("2160p"));

        // Plain 1080p still works
        let parsed = parse_quality("Movie.2024.1080p.WEB-DL.x265");
        assert_eq!(parsed.resolution.as_deref(), Some("1080p"));
    }

    #[test]
    fn test_framerate_suffixed_resolution_scoring() {
        // 1080p50 should get the same quality score bonuses as 1080p
        let score_fps = score_quality(
            "MotoGP 2026-Season Launch-1080p50 DornaRip HEVC AAC x265-deef",
            None,
            None,
            None,
        );
        let score_plain = score_quality(
            "MotoGP 2026-Season Launch-1080p HEVC AAC x265-deef",
            None,
            None,
            None,
        );
        assert_eq!(
            score_fps.breakdown.resolution_tier, score_plain.breakdown.resolution_tier,
            "1080p50 should get same resolution tier bonus as 1080p"
        );
        assert_eq!(
            score_fps.parsed.resolution.as_deref(),
            Some("1080p"),
            "1080p50 should parse as 1080p"
        );
    }
}
