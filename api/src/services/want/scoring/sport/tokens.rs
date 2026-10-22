use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::LazyLock;

use regex::{Captures, Regex};
use serde_json::Value;

use crate::parsing::canonical_sport_text;
use crate::quality::parse_quality;

#[derive(Debug, Clone)]
pub struct Token {
    pub text: String,
    pub quality: bool,
    pub strong_quality: bool,
    pub group: bool,
}

static EXTENSION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(?i)\.(mkv|mp4|avi|ts|m4v|wmv|epub|mobi|azw3?|kepub|pdf|cbz|cbr|m4b|mp3|nzb|torrent)\s*$",
    )
    .unwrap()
});
static SITE_PREFIX: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^\s*(?:www\.[^\s]+\s*-\s*|\[[^\]]*\.(?:site|xyz|org|com|net|bar|to)\]\s*)")
        .unwrap()
});
static PART_PREFIX: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^\s*\[\d+/\d+\]\s*-?\s*").unwrap());
static APOS_ENTITY: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"&?#0*39;?").unwrap());
static QUOT_ENTITY: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"&quot;|(^|[\s\-\]\[])quot([A-Z])").unwrap());
static DATE_YMD: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(20\d{2})[. -](0[1-9]|1[0-2])[. -](0[1-9]|[12]\d|3[01])\b").unwrap()
});
static DATE_DMY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"\b(0?[1-9]|[12]\d|3[01])[. -](0?[1-9]|1[0-2])[. -](20\d{2})\b").unwrap()
});
static DATE_DMY_SHORT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(0[1-9]|[12]\d|3[01])\.(0[1-9]|1[0-2])\.(2\d)\b").unwrap());
static DATE_COMPACT: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\b(20\d{2})(0[1-9]|1[0-2])(0[1-9]|[12]\d|3[01])\b").unwrap());
static SEPARATORS: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"[\s._\-\[\](){}+,|~]+").unwrap());
static CAMEL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"([a-z])([A-Z][a-z])").unwrap());
static GLUED_GP: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(.{3,}?)GP$").unwrap());
static GLUED_RES: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"(?i)^([a-z]{3,})(\d{3,4}p\d*)$").unwrap());
static EXTRA_QUALITY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:\d{3,4}[pi]?\d{0,2}|\d{2,3}fps|hfr|sdr|8bit|10bit|10b|50f|opus|2ch|6ch|hd|fhd|uhd|4k|mkv|mp4)$")
        .unwrap()
});
static RESOLUTION_LIKE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^(\d{3,4})[pi]?\d{0,2}$").unwrap());

/// Dates become one `zdYYYYMMDD` token, whichever order the release wrote
/// them in. No scene word starts "zd".
fn date_marker(y: i32, m: u32, d: u32) -> String {
    format!("zd{y:04}{m:02}{d:02}")
}

pub fn parse_date_marker(t: &str) -> Option<time::Date> {
    let digits = t.strip_prefix("zd")?;
    if digits.len() != 8 {
        return None;
    }
    let y: i32 = digits[..4].parse().ok()?;
    let m: u8 = digits[4..6].parse().ok()?;
    let d: u8 = digits[6..].parse().ok()?;
    time::Date::from_calendar_date(y, time::Month::try_from(m).ok()?, d).ok()
}

fn replace_dates(s: &str) -> String {
    let valid = |y: i32, m: u32, d: u32| -> Option<String> {
        time::Date::from_calendar_date(y, time::Month::try_from(m as u8).ok()?, d as u8).ok()?;
        Some(format!(" {} ", date_marker(y, m, d)))
    };
    let num = |c: &Captures, i: usize| c[i].parse::<u32>().unwrap_or(0);
    let s = DATE_YMD.replace_all(s, |c: &Captures| {
        valid(num(c, 1) as i32, num(c, 2), num(c, 3)).unwrap_or_else(|| c[0].to_string())
    });
    let s = DATE_DMY.replace_all(&s, |c: &Captures| {
        valid(num(c, 3) as i32, num(c, 2), num(c, 1)).unwrap_or_else(|| c[0].to_string())
    });
    let s = DATE_DMY_SHORT.replace_all(&s, |c: &Captures| {
        valid(2000 + num(c, 3) as i32, num(c, 2), num(c, 1)).unwrap_or_else(|| c[0].to_string())
    });
    DATE_COMPACT
        .replace_all(&s, |c: &Captures| {
            valid(num(c, 1) as i32, num(c, 2), num(c, 3)).unwrap_or_else(|| c[0].to_string())
        })
        .into_owned()
}

const QUALITY_CACHE_LIMIT: usize = 50_000;

thread_local! {
    static QUALITY_CACHE: RefCell<HashMap<String, Value>> = RefCell::new(HashMap::new());
}

/// Releases repeat the same few hundred words, so it is cached per thread;
/// the cap keeps a long-running process from growing it without bound.
fn quality_signature(s: &str) -> Value {
    QUALITY_CACHE.with(|cache| {
        if let Some(v) = cache.borrow().get(s) {
            return v.clone();
        }
        let mut v = serde_json::to_value(parse_quality(s)).unwrap_or(Value::Null);
        if let Some(obj) = v.as_object_mut() {
            obj.remove("release_group");
        }
        let mut cache = cache.borrow_mut();
        if cache.len() >= QUALITY_CACHE_LIMIT {
            cache.clear();
        }
        cache.insert(s.to_string(), v.clone());
        v
    })
}

fn is_empty_signature(v: &Value) -> bool {
    v.as_object().is_none_or(|o| o.is_empty())
}

const STRONG_KEYS: &[&str] = &[
    "resolution",
    "source",
    "streaming_service",
    "codec",
    "audio",
    "is_hdr",
    "is_hdr10",
    "is_hdr10plus",
    "is_dolby_vision",
    "is_hlg",
    "is_atmos",
    "is_dtsx",
    "is_3d",
    "is_remux",
];

fn resolution_like(s: &str) -> Option<bool> {
    RESOLUTION_LIKE
        .captures(s)
        .map(|c| c[1].parse::<u32>().is_ok_and(|n| n >= 360))
}

fn is_quality_word(s: &str) -> bool {
    resolution_like(s)
        .unwrap_or_else(|| EXTRA_QUALITY.is_match(s) || !is_empty_signature(&quality_signature(s)))
}

fn is_strong_quality_word(s: &str) -> bool {
    resolution_like(s).unwrap_or_else(|| {
        EXTRA_QUALITY.is_match(s)
            || quality_signature(s)
                .as_object()
                .is_some_and(|o| o.keys().any(|k| STRONG_KEYS.contains(&k.as_str())))
    })
}

fn split_segment(seg: &str) -> String {
    if seg.eq_ignore_ascii_case("motogp") || is_quality_word(&seg.to_lowercase()) {
        return seg.to_string();
    }
    if let Some(c) = GLUED_RES.captures(seg) {
        return format!("{} {}", &c[1], &c[2]);
    }
    let seg = match GLUED_GP.captures(seg) {
        Some(c) => format!("{} GP", &c[1]),
        None => seg.to_string(),
    };
    CAMEL.replace_all(&seg, "$1 $2").into_owned()
}

fn strip_noise(title: &str) -> String {
    let s = APOS_ENTITY.replace_all(title, "'");
    let s = QUOT_ENTITY.replace_all(&s, "$1$2");
    let s = PART_PREFIX.replace(&s, "");
    let mut s = EXTENSION.replace(&s, "").into_owned();
    while let Some(m) = SITE_PREFIX.find(&s) {
        if m.end() == 0 {
            break;
        }
        s = s[m.end()..].to_string();
    }
    s
}

/// Words of a want-side name, spelt the way [`tokenize`] spells a release.
pub fn normalize_phrase(s: &str) -> Vec<String> {
    let joined = SEPARATORS
        .split(s)
        .filter(|p| !p.is_empty())
        .map(split_segment)
        .collect::<Vec<_>>()
        .join(" ");
    canonical_sport_text(&joined)
        .split_whitespace()
        .map(String::from)
        .collect()
}

pub fn tokenize(title: &str) -> Vec<Token> {
    let cleaned = strip_noise(title).replace('_', " ");
    let mut tokens: Vec<Token> = normalize_phrase(&replace_dates(&cleaned))
        .into_iter()
        .map(|t| {
            let marker = t.starts_with("zd");
            Token {
                quality: !marker && is_quality_word(&t),
                strong_quality: !marker && is_strong_quality_word(&t),
                group: false,
                text: t,
            }
        })
        .collect();
    mark_quality_pairs(&mut tokens);
    mark_group(&mut tokens, title);
    tokens
}

/// "WEB DL", "H 264", "DDP5 1": words that only read as quality together.
fn mark_quality_pairs(tokens: &mut [Token]) {
    for i in 0..tokens.len().saturating_sub(1) {
        let (a, b) = (&tokens[i], &tokens[i + 1]);
        if (a.quality && b.quality) || a.text.starts_with("zd") || b.text.starts_with("zd") {
            continue;
        }
        for sep in [".", "-"] {
            let joined = quality_signature(&format!("{}{sep}{}", a.text, b.text));
            if is_empty_signature(&joined) {
                continue;
            }
            let sa = quality_signature(&a.text);
            let sb = quality_signature(&b.text);
            let reference = if is_empty_signature(&sa) { &sb } else { &sa };
            if (is_empty_signature(&sa) || is_empty_signature(&sb)) && &joined != reference {
                for t in &mut tokens[i..=i + 1] {
                    t.quality = true;
                    t.strong_quality = true;
                }
                break;
            }
        }
    }
}

fn mark_group(tokens: &mut [Token], title: &str) {
    let Some(group) = parse_quality(&strip_noise(title)).release_group else {
        return;
    };
    let group_tokens = normalize_phrase(&group);
    if group_tokens.is_empty() || group_tokens.len() > tokens.len() {
        return;
    }
    let start = tokens.len() - group_tokens.len();
    if tokens[start..]
        .iter()
        .zip(&group_tokens)
        .all(|(t, g)| &t.text == g)
    {
        for t in &mut tokens[start..] {
            t.group = true;
        }
    }
}
