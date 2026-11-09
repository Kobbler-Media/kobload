//! One Aho-Corasick pass finds every occurrence of every word `parse_quality`
//! looks for, so each `has` is a lookup over those occurrences rather than a
//! fresh substring search of the title (89% of `parse_quality` was building
//! and running those searches).

use std::collections::HashMap;
use std::sync::LazyLock;

use aho_corasick::AhoCorasick;

use super::{FORMATS_3D, RESOLUTIONS, STREAMING_SERVICES, VIDEO_CODECS, has_word, word_at};

/// The literal words passed to `has` in this module. Kept in step with the
/// source by `registry_covers_every_literal_word`; a word missing here still
/// works, through the slow path.
const WORDS: &[&str] = &[
    "atmos",
    "dts-x",
    "dtsx",
    "truehd",
    "true-hd",
    "mlp",
    "pcm",
    "lpcm",
    "flac",
    "dts",
    "ddp",
    "eac3",
    "e-ac3",
    "ac3",
    "ac-3",
    "aac",
    "mp3",
    "opus",
    "vorbis",
    "ogg",
    "wma",
    "proper",
    "repack",
    "rerip",
    "real",
    "remux",
    "internal",
    "retail",
    "limited",
    "nfofix",
    "nfo.fix",
    "samplefix",
    "sample.fix",
    "syncfix",
    "sync.fix",
    "audiofix",
    "audio.fix",
    "proof",
    "dirfix",
    "dir.fix",
    "dubbed",
    "dub",
    "subbed",
    "subs",
    "hc",
    "hardcoded",
    "hardsub",
    "hardsubs",
    "korsub",
    "chtsub",
    "subtitulado",
    "subtitulada",
    "vose",
    "dolby.atmos",
    "dts:x",
    "nuked",
    "nuke",
    "bluray",
    "blu-ray",
    "bdrip",
    "brrip",
    "bd25",
    "bd50",
    "web-dl",
    "webdl",
    "web.dl",
    "webrip",
    "web-rip",
    "web.rip",
    "web",
    "hdtv",
    "hd-tv",
    "pdtv",
    "dsr",
    "dsrip",
    "sdtv",
    "tvrip",
    "tv-rip",
    "sd-tv",
    "satrip",
    "satellite",
    "dvdrip",
    "dvd-rip",
    "dvdr",
    "dvd-r",
    "dvd5",
    "dvd9",
    "dvdscr",
    "dvd-screener",
    "dvd",
    "bdscr",
    "bluray-screener",
    "screener",
    "scr",
    "r5",
    "telecine",
    "tc",
    "cam",
    "camrip",
    "hdcam",
    "workprint",
    "wp",
    "ppvrip",
    "ppv",
    "vod",
    "vodrip",
    "hddvd",
    "hd-dvd",
    "dv",
    "dovi",
    "dolby.vision",
    "dolbyvision",
    "hdr10plus",
    "hdr10+",
    "hdr10",
    "hlg",
    "hdr",
];

struct Registry {
    anchors: AhoCorasick,
    /// word -> (anchor id, anchor length, the `.`-separated rest after it)
    words: HashMap<&'static str, (u32, usize, Option<&'static str>)>,
}

static REGISTRY: LazyLock<Registry> = LazyLock::new(|| {
    let tables = STREAMING_SERVICES
        .iter()
        .chain(VIDEO_CODECS)
        .chain(FORMATS_3D)
        .flat_map(|(aliases, _)| aliases.iter())
        .chain(
            RESOLUTIONS
                .iter()
                .flat_map(|(aliases, _, _)| aliases.iter()),
        )
        .copied()
        .chain(crate::language::title_words());
    let mut anchor_ids: HashMap<&'static str, u32> = HashMap::new();
    let mut anchors = Vec::new();
    let mut words = HashMap::new();
    for word in WORDS.iter().copied().chain(tables) {
        let (anchor, rest) = word
            .split_once('.')
            .map_or((word, None), |(a, _)| (a, Some(&word[a.len()..])));
        if anchor.is_empty() {
            continue;
        }
        let id = *anchor_ids.entry(anchor).or_insert_with(|| {
            anchors.push(anchor);
            anchors.len() as u32 - 1
        });
        words.insert(word, (id, anchor.len(), rest));
    }
    Registry {
        anchors: AhoCorasick::new(&anchors).expect("quality word automaton"),
        words,
    }
});

/// A lowercased title with every occurrence of every registered word anchor,
/// as (anchor id, start) sorted by anchor id.
pub(super) struct TitleWords<'a> {
    lower: &'a str,
    hits: Vec<(u32, usize)>,
}

impl<'a> TitleWords<'a> {
    pub(super) fn new(lower: &'a str) -> Self {
        let mut hits: Vec<(u32, usize)> = REGISTRY
            .anchors
            .find_overlapping_iter(lower)
            .map(|m| (m.pattern().as_u32(), m.start()))
            .collect();
        hits.sort_unstable();
        Self { lower, hits }
    }

    pub(super) fn lower(&self) -> &'a str {
        self.lower
    }

    /// Same answer as `has_word(self.lower(), word)`.
    pub(super) fn has(&self, word: &str) -> bool {
        let Some(&(id, anchor_len, rest)) = REGISTRY.words.get(word) else {
            return has_word(self.lower, word);
        };
        let from = self.hits.partition_point(|&(i, _)| i < id);
        self.hits[from..]
            .iter()
            .take_while(|&&(i, _)| i == id)
            .any(|&(_, start)| word_at(self.lower, start, anchor_len, rest))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_covers_every_literal_word() {
        let source = include_str!("../quality.rs");
        let source = &source[..source.find("#[cfg(test)]").unwrap_or(source.len())];
        let literal = regex::Regex::new(r#"words\.has\("([^"]*)"\)"#).unwrap();
        let missing: Vec<&str> = literal
            .captures_iter(source)
            .map(|c| c.get(1).unwrap().as_str())
            .filter(|w| !REGISTRY.words.contains_key(w))
            .collect();
        assert!(missing.is_empty(), "add to WORDS: {missing:?}");
    }
}
