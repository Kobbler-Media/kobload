//! Shared title tokenization for relevance / overlap checks.
//!
//! Splits on non-alphanumeric characters, drops short tokens and common
//! stopwords, and lowercases everything. Used by indexer-result relevance and
//! by the koblem-review shortlist filter.

pub(crate) const TITLE_STOPWORDS: &[&str] = &[
    "the", "a", "an", "and", "or", "of", "in", "on", "at", "to", "for", "is", "it", "by", "with",
    "from", "as", "but", "not", "no", "be", "was", "are", "der", "die", "das", "le", "la", "les",
    "el", "los", "las", "de", "du", "des", "un", "une",
];

/// Extract significant words (≥2 chars, not stopwords) from a title, lowercased.
pub(crate) fn title_keywords(title: &str) -> Vec<String> {
    title
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.len() >= 2)
        .map(|w| w.to_lowercase())
        .filter(|w| !TITLE_STOPWORDS.contains(&w.as_str()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_stopwords_and_short_tokens() {
        let kw = title_keywords("Malcolm in the Middle: Life's Still Unfair");
        assert!(kw.contains(&"malcolm".to_string()));
        assert!(kw.contains(&"middle".to_string()));
        assert!(kw.contains(&"life".to_string()));
        assert!(kw.contains(&"still".to_string()));
        assert!(kw.contains(&"unfair".to_string()));
        assert!(!kw.iter().any(|w| w == "in" || w == "the"));
    }
}
