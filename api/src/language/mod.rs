mod audio;
mod table;

pub use audio::{LanguageFit, ReleaseLanguages, fit, parse_audio};
use table::{LANGUAGES, Language};

const MULTI: &[&str] = &["multi", "dual"];

fn resolve(spelling: &str) -> Option<&'static Language> {
    let spelling = spelling.trim().to_lowercase();
    LANGUAGES
        .iter()
        .find(|l| l.spellings().any(|s| s == spelling))
}

/// ISO 639-1 codes for the caller's languages, in their order; unknown entries are dropped.
pub fn normalise(preferred: &[String]) -> Vec<&'static str> {
    let mut out: Vec<&'static str> = Vec::new();
    for code in preferred
        .iter()
        .filter_map(|raw| resolve(raw))
        .map(|l| l.code)
    {
        if !out.contains(&code) {
            out.push(code);
        }
    }
    out
}

/// The name quality profiles compare on: "multi" for multi-language markers, otherwise
/// the language's English name.
pub fn profile_name(spelling: &str) -> Option<&'static str> {
    if MULTI.contains(&spelling.trim().to_lowercase().as_str()) {
        return Some("multi");
    }
    resolve(spelling).map(|l| l.name)
}

fn profile_tokens(l: &'static Language) -> impl Iterator<Item = &'static str> {
    l.tags.iter().chain(l.profile_tags).copied()
}

/// Every word `title_language` may test for.
pub fn title_words() -> impl Iterator<Item = &'static str> {
    MULTI.iter().copied().chain(
        LANGUAGES
            .iter()
            .flat_map(|l| std::iter::once(l.name).chain(profile_tokens(l))),
    )
}

/// Full names win over tags, so "FRENCH ... ENG SUBS" reads as French.
pub fn title_language(has: impl Fn(&str) -> bool) -> Option<&'static str> {
    if MULTI.iter().any(|w| has(w)) {
        return Some("multi");
    }
    LANGUAGES
        .iter()
        .find(|l| has(l.name))
        .or_else(|| LANGUAGES.iter().find(|l| profile_tokens(l).any(&has)))
        .map(|l| l.name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalises_names_and_iso_codes() {
        let input: Vec<String> = ["English", "ITA", "de", "ger", " fre ", "klingon"]
            .map(String::from)
            .to_vec();
        assert_eq!(normalise(&input), vec!["en", "it", "de", "fr"]);
    }

    #[test]
    fn every_spelling_resolves_to_its_own_row() {
        for l in LANGUAGES {
            for spelling in l.spellings().filter(|s| *s != l.code) {
                let found = resolve(spelling).map(|r| r.name);
                assert_eq!(found, Some(l.name), "{spelling}");
            }
        }
    }

    #[test]
    fn title_language_prefers_full_names_then_table_order() {
        let cases: &[(&[&str], Option<&str>)] = &[
            (&["french", "eng"], Some("french")),
            (&["ita", "eng"], Some("english")),
            (&["dual", "french"], Some("multi")),
            (&["vostfr"], Some("french")),
            (&["cantonese"], Some("cantonese")),
            (&["mandarin"], Some("chinese")),
            (&["per"], None),
            (&["vff"], Some("french")),
        ];
        for (words, want) in cases {
            assert_eq!(title_language(|w| words.contains(&w)), *want, "{words:?}");
        }
    }
}
