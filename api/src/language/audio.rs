use super::MULTI;
use super::table::{LANGUAGES, Language};

const SUBTITLE_WORDS: &[&str] = &[
    "sub",
    "subs",
    "subbed",
    "subtitle",
    "subtitles",
    "subtitled",
];

#[derive(Debug, Default, PartialEq, Eq)]
pub struct ReleaseLanguages {
    pub listed: Vec<&'static str>,
    pub multi: bool,
}

fn is_anchor(token: &str) -> bool {
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    let year =
        token.len() == 4 && digits(token) && (token.starts_with("19") || token.starts_with("20"));
    let resolution = token.len() >= 4
        && (token.ends_with('p') || token.ends_with('i'))
        && digits(&token[..token.len() - 1]);
    let season = token
        .strip_prefix('s')
        .map(|rest| {
            let (s, e) = rest.split_once('e').unwrap_or((rest, "0"));
            digits(s) && s.len() <= 2 && digits(e)
        })
        .unwrap_or(false);
    let numbered = token
        .split_once('x')
        .is_some_and(|(s, e)| digits(s) && s.len() <= 2 && digits(e));
    year || resolution || season || numbered || token == "4k"
}

fn names_audio(l: &Language, token: &str) -> bool {
    l.name == token || l.tags.contains(&token) || l.stream_tags.contains(&token)
}

/// Languages are read only after the first year, episode or resolution token, so a title
/// like "The.Italian.Job" is not taken for a language tag.
pub fn parse_audio(title: &str) -> ReleaseLanguages {
    let lower = title.to_lowercase();
    let tokens: Vec<&str> = lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| !t.is_empty())
        .collect();
    let Some(anchor) = tokens.iter().position(|t| is_anchor(t)) else {
        return ReleaseLanguages::default();
    };
    let subtitle = |i: usize| {
        let near = |j: Option<usize>| {
            j.and_then(|j| tokens.get(j))
                .is_some_and(|t| SUBTITLE_WORDS.contains(t))
        };
        near(i.checked_sub(1)) || near(Some(i + 1))
    };
    let mut out = ReleaseLanguages::default();
    for (i, token) in tokens.iter().enumerate().skip(anchor + 1) {
        if MULTI.contains(token) {
            out.multi = true;
            continue;
        }
        let Some(l) = LANGUAGES.iter().find(|l| names_audio(l, token)) else {
            continue;
        };
        if !subtitle(i) && !out.listed.contains(&l.code) {
            out.listed.push(l.code);
        }
    }
    out
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LanguageFit {
    /// The preferred language is the default track, or nothing says otherwise. An untagged
    /// release is in its original language, which is what most people want.
    Default,
    /// The preferred language is likely there, but not as the default track.
    Secondary,
    Missing,
}

pub fn fit(release: &ReleaseLanguages, preferred: &[&str]) -> LanguageFit {
    let wanted = |code: &&str| preferred.contains(code);
    match release.listed.first() {
        _ if preferred.is_empty() => LanguageFit::Default,
        None => LanguageFit::Default,
        Some(first) if wanted(first) => LanguageFit::Default,
        _ if release.listed.iter().any(wanted) || release.multi => LanguageFit::Secondary,
        _ => LanguageFit::Missing,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_language_tags_in_release_order() {
        let cases: &[(&str, &[&str], bool)] = &[
            (
                "Altered.Carbon.1x01.Le.Catene.Della.Colpa.ITA.ENG.1080p.NFRip.AAC.x265-Pir8",
                &["it", "en"],
                false,
            ),
            (
                "Movie.2020.iTA.ENG.AC3.1080p.BluRay-G",
                &["it", "en"],
                false,
            ),
            ("The.Italian.Job.2003.1080p.BluRay.x264-G", &[], false),
            ("Show.S01E01.1080p.VOSTFR.WEB-G", &[], false),
            ("Movie.2019.FRENCH.1080p.WEB-G", &["fr"], false),
            ("Movie.2019.TRUEFRENCH.1080p.WEB-G", &["fr"], false),
            ("Movie.2020.LATINO.1080p.WEB-G", &["es"], false),
            ("Movie.2020.German.DL.1080p.BluRay-G", &["de"], false),
            ("Movie.2020.1080p.MULTI.WEB-G", &[], true),
            ("Movie.2020.FRENCH.DUAL.1080p.WEB-G", &["fr"], true),
            ("Movie.2020.1080p.WEB.ENG.SUBS-G", &[], false),
            ("Movie.2020.1080p.WEB.SUB.ITA-G", &[], false),
            ("Movie.2020.1080p.CANTONESE.WEB-G", &["zh"], false),
            ("Show.S01E01.Per.Fin.1080p.WEB-G", &[], false),
            ("Movie.ITA", &[], false),
        ];
        for (title, listed, multi) in cases {
            let parsed = parse_audio(title);
            assert_eq!(parsed.listed, *listed, "{title}");
            assert_eq!(parsed.multi, *multi, "{title}");
        }
    }

    #[test]
    fn fit_ranks_default_track_over_secondary_over_missing() {
        let release = |listed: &[&'static str], multi| ReleaseLanguages {
            listed: listed.to_vec(),
            multi,
        };
        let cases = [
            (
                release(&["it", "en"], false),
                &["en"][..],
                LanguageFit::Secondary,
            ),
            (
                release(&["en", "it"], false),
                &["en"][..],
                LanguageFit::Default,
            ),
            (release(&["it"], false), &["en"][..], LanguageFit::Missing),
            (release(&["it"], false), &[][..], LanguageFit::Default),
            (release(&[], false), &["en"][..], LanguageFit::Default),
            (release(&[], true), &["en"][..], LanguageFit::Default),
            (release(&["fr"], true), &["en"][..], LanguageFit::Secondary),
            (
                release(&["it"], false),
                &["de", "it"][..],
                LanguageFit::Default,
            ),
        ];
        for (release, preferred, want) in cases {
            assert_eq!(fit(&release, preferred), want, "{release:?} {preferred:?}");
        }
    }
}
