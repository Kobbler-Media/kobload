use kob_normalize::{canonical, contains_pattern};

use super::types::{MatchResult, MatchTarget, WatchPattern};

/// Check if a token looks like a structured season/episode identifier
/// (e.g. "s06", "s01e03", "1x07").
fn is_season_episode_token(t: &str) -> bool {
    let t = canonical(t);
    let bytes = t.as_bytes();
    if bytes.is_empty() {
        return false;
    }
    // s01, s01e03, s1e3, etc.
    if bytes[0] == b's' && bytes.len() >= 2 && bytes[1].is_ascii_digit() {
        return true;
    }
    // 1x07 format
    if bytes[0].is_ascii_digit() && t.contains('x') {
        let parts: Vec<&str> = t.splitn(2, 'x').collect();
        if parts.len() == 2
            && parts[0].chars().all(|c| c.is_ascii_digit())
            && parts[1].chars().all(|c| c.is_ascii_digit())
        {
            return true;
        }
    }
    false
}

/// Result of scoring a release against match targets
#[derive(Debug, Clone)]
pub struct TargetMatchResult {
    /// Score against the best matching target
    pub score: f64,
    /// The target string that was matched
    pub matched_target: String,
    /// Structured identification of what was matched.
    pub semantic_target: Option<nats_jobs::MatchedTarget>,
}

/// A release title canonicalised once, for scoring against every target of
/// every pattern in a fetch.
pub struct PreparedTitle<'t> {
    title: &'t str,
    text: TextTokens,
    sport: std::cell::OnceCell<SportTitle>,
    sport_tokens: std::cell::OnceCell<Vec<crate::services::want::scoring::SportToken>>,
}

struct SportTitle {
    words: Vec<String>,
    text: TextTokens,
}

impl<'t> PreparedTitle<'t> {
    pub fn new(title: &'t str) -> Self {
        Self {
            title,
            text: TextTokens::new(&title.to_lowercase()),
            sport: std::cell::OnceCell::new(),
            sport_tokens: std::cell::OnceCell::new(),
        }
    }

    pub(crate) fn sport_tokens(&self) -> &[crate::services::want::scoring::SportToken] {
        self.sport_tokens
            .get_or_init(|| crate::services::want::scoring::tokenize_sport(self.title))
    }

    fn sport(&self) -> &SportTitle {
        self.sport.get_or_init(|| {
            let canon = crate::parsing::canonical_sport_text(self.title);
            SportTitle {
                words: canon.split_whitespace().map(String::from).collect(),
                text: TextTokens::new(&canon),
            }
        })
    }
}

/// A match target canonicalised once per pattern.
pub struct PreparedTarget<'a> {
    target: &'a MatchTarget,
    text: PatternTokens,
    sport: Option<SportTarget>,
}

/// Sport wants and releases spell rounds, session numbers and places
/// differently ("R12 ... Race 1" vs "Round12 ... Race.One"), so sport targets
/// are also scored in canonical form against the canonical title.
///
/// Nothing downstream checks the league: once the round, place and session line
/// up, only the league token keeps a Moto2 want off the MotoGP release of the
/// same session, and an SBK want off World Supersport ("WSSP"). A token or two
/// short of a perfect score would still clear the floor, so the league has to
/// be present outright. It is everything before the season year in the target.
struct SportTarget {
    league: Vec<String>,
    text: PatternTokens,
}

impl<'a> PreparedTarget<'a> {
    pub fn new(target: &'a MatchTarget) -> Self {
        let sport = matches!(
            target.semantic_target,
            Some(nats_jobs::MatchedTarget::SportEvent)
        )
        .then(|| {
            let canon = crate::parsing::canonical_sport_text(&target.target);
            let league = canon
                .split_whitespace()
                .take_while(|t| !(t.len() == 4 && t.bytes().all(|b| b.is_ascii_digit())))
                .map(String::from)
                .collect();
            SportTarget {
                league,
                text: PatternTokens::new(&canon, &[]),
            }
        })
        .filter(|s| !s.league.is_empty());
        Self {
            target,
            text: PatternTokens::new(&target.target.to_lowercase(), &[]),
            sport,
        }
    }
}

pub fn prepare_targets(targets: &[MatchTarget]) -> Vec<PreparedTarget<'_>> {
    targets.iter().map(PreparedTarget::new).collect()
}

/// Score a release title against multiple match targets.
/// Returns the best matching target (highest score above threshold).
pub fn score_against_targets(
    release_title: &str,
    targets: &[MatchTarget],
    default_min_score: f64,
) -> Option<TargetMatchResult> {
    score_prepared_targets(
        &PreparedTitle::new(release_title),
        &prepare_targets(targets),
        default_min_score,
    )
}

pub fn score_prepared_targets(
    title: &PreparedTitle,
    targets: &[PreparedTarget],
    default_min_score: f64,
) -> Option<TargetMatchResult> {
    targets
        .iter()
        .filter_map(|prepared| {
            let target = prepared.target;
            let mut score = similarity(&prepared.text, &title.text);
            if let Some(sport) = &prepared.sport {
                let sport_title = title.sport();
                if sport
                    .league
                    .iter()
                    .all(|l| sport_title.words.iter().any(|w| w == l))
                {
                    score = score.max(similarity(&sport.text, &sport_title.text));
                }
            }
            let min_score = target.min_score.unwrap_or(default_min_score);

            if score >= min_score {
                Some(TargetMatchResult {
                    score,
                    matched_target: target.target.clone(),
                    semantic_target: target.semantic_target.clone(),
                })
            } else {
                None
            }
        })
        .max_by(|a, b| {
            a.score
                .partial_cmp(&b.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
}

/// Match a release title against a watch pattern
pub fn match_release(
    pattern: &WatchPattern,
    release_title: &str,
    _release_category_ids: &[i32],
) -> Option<MatchResult> {
    // Note: Category filtering is not done here directly.
    // The caller should filter categories before calling this function.
    // This function only handles text pattern matching.

    let title_lower = release_title.to_lowercase();
    let min_score = pattern.min_score.unwrap_or(0.75);

    match pattern.match_mode.as_str() {
        "any" => match_any(&pattern.patterns, &title_lower),
        "all" => match_all(&pattern.patterns, &title_lower),
        "fuzzy" => match_fuzzy(&pattern.patterns, &title_lower, min_score),
        _ => match_fuzzy(&pattern.patterns, &title_lower, min_score), // default to fuzzy
    }
}

/// Match a release title against patterns (for search-and-download flow)
pub fn match_patterns(
    patterns: &[String],
    match_mode: &str,
    min_score: f64,
    release_title: &str,
) -> Option<MatchResult> {
    let title_lower = crate::parsing::common::repair_apostrophes(release_title).to_lowercase();

    match match_mode {
        "any" => match_any(patterns, &title_lower),
        "all" => match_all(patterns, &title_lower),
        "fuzzy" => match_fuzzy(patterns, &title_lower, min_score),
        _ => match_fuzzy(patterns, &title_lower, min_score),
    }
}

/// Any pattern matches (OR logic).
/// Uses `kob_normalize::contains_pattern` so scene-release ambiguity
/// (apostrophe/hyphen dropped vs as-separator, diacritics, smart quotes,
/// ampersand) is handled once, centrally.
fn match_any(patterns: &[String], title: &str) -> Option<MatchResult> {
    for p in patterns {
        if contains_pattern(title, p) {
            return Some(MatchResult {
                score: 1.0,
                matched_pattern: p.clone(),
            });
        }
    }
    None
}

/// All patterns must match (AND logic).
fn match_all(patterns: &[String], title: &str) -> Option<MatchResult> {
    if patterns.iter().all(|p| contains_pattern(title, p)) {
        Some(MatchResult {
            score: 1.0,
            matched_pattern: patterns.join(" + "),
        })
    } else {
        None
    }
}

/// Fuzzy matching with scoring - tries each pattern individually and returns the best match
fn match_fuzzy(patterns: &[String], title_lower: &str, min_score: f64) -> Option<MatchResult> {
    let mut best_result: Option<MatchResult> = None;

    for pattern in patterns {
        let pattern_lower = pattern.to_lowercase();
        let score = fuzzy_score(&pattern_lower, title_lower);

        if score >= min_score {
            match &best_result {
                None => {
                    best_result = Some(MatchResult {
                        score,
                        matched_pattern: pattern.clone(),
                    });
                }
                Some(existing) if score > existing.score => {
                    best_result = Some(MatchResult {
                        score,
                        matched_pattern: pattern.clone(),
                    });
                }
                _ => {}
            }
        }
    }

    best_result
}

/// Sanity check for Jaro-Winkler matches: tokens with identical letters
/// but different digits are structured identifier mismatches, not typos.
/// e.g., "s01e01" vs "s01e07" share letters "se" but digits "0101" ≠ "0107".
fn jw_digit_sanity_check(a: &str, b: &str) -> bool {
    let a_alpha: String = a.chars().filter(|c| !c.is_ascii_digit()).collect();
    let b_alpha: String = b.chars().filter(|c| !c.is_ascii_digit()).collect();
    if a_alpha != b_alpha {
        return true; // different letters — this is a legitimate fuzzy match
    }
    let a_digits: String = a.chars().filter(|c| c.is_ascii_digit()).collect();
    let b_digits: String = b.chars().filter(|c| c.is_ascii_digit()).collect();
    // Same letters + non-empty digits + different digits → structured mismatch
    a_digits.is_empty() || a_digits == b_digits
}

/// Calculate a fuzzy match score between a pattern and text.
/// Thin wrapper that preserves the original two-argument signature for watch
/// folder / download matching. Callers who want to drop stopwords from the
/// pattern side (e.g. the koblem-review shortlist filter) should call
/// [`title_similarity`] directly.
fn fuzzy_score(pattern: &str, text: &str) -> f64 {
    title_similarity(pattern, text, &[])
}

// Canonical pattern tokens. Single-digit numbers (sequel numbers like "5")
// are kept but other single-char tokens dropped; canonicalisation has already
// removed punctuation, diacritics and smart quotes and expanded ampersands.
struct PatternTokens {
    norm: String,
    tokens: Vec<String>,
}

impl PatternTokens {
    fn new(pattern: &str, stopwords: &[&str]) -> Self {
        let norm = canonical(pattern);
        let tokens = norm
            .split_whitespace()
            .filter(|t| t.len() >= 2 || t.chars().all(|c| c.is_ascii_digit()))
            .filter(|t| !stopwords.contains(t))
            .map(String::from)
            .collect();
        Self { norm, tokens }
    }
}

// Canonical release-side tokens. The release group is stripped first so group
// names like "samhydetorrentgalaxy" can't pollute token matching, and
// canonicalising splits dot/underscore/hyphen-separated titles into words.
struct TextTokens {
    norm: String,
    tokens: Vec<String>,
}

impl TextTokens {
    fn new(text: &str) -> Self {
        let repaired = crate::parsing::common::repair_apostrophes(text);
        let stripped = crate::parsing::common::RELEASE_GROUP_RE.replace(&repaired, "");
        let norm = rejoin_possessive_s(&canonical(&stripped));
        let tokens = norm
            .split_whitespace()
            .filter(|t| t.len() >= 2 || t.chars().all(|c| c.is_ascii_digit()))
            .filter(|t| !t.starts_with('[') && !t.ends_with(']'))
            .map(String::from)
            .collect();
        Self { norm, tokens }
    }
}

/// Releases that replaced an apostrophe with a separator ("Grey s Anatomy",
/// "Grey_s.Anatomy") leave a lone "s" where canonical() of the wanted title
/// has "greys". A lone "s" before a number is a season marker ("S 01"),
/// before another single letter it is part of an initialism ("S.H.I.E.L.D"),
/// and at the end of a title it is no possessive, so those stay.
pub(super) fn rejoin_possessive_s(norm: &str) -> String {
    let words: Vec<&str> = norm.split(' ').collect();
    let mut out = String::with_capacity(norm.len());
    for (i, word) in words.iter().enumerate() {
        let joins = *word == "s"
            && i > 0
            && words[i - 1].len() > 1
            && words[i - 1].chars().all(char::is_alphabetic)
            && words.get(i + 1).is_some_and(|next| {
                next.len() > 1 && !next.starts_with(|c: char| c.is_ascii_digit())
            });
        if !joins && !out.is_empty() {
            out.push(' ');
        }
        out.push_str(word);
    }
    out
}

/// Calculate a fuzzy match score between a pattern and text, dropping any
/// pattern tokens that match `stopwords` so articles like "the"/"in" don't
/// pad the match denominator. Uses a combination of techniques:
/// 1. Token overlap (words in common)
/// 2. Substring matching for each token
/// 3. Jaro-Winkler similarity for short patterns
pub(crate) fn title_similarity(pattern: &str, text: &str, stopwords: &[&str]) -> f64 {
    similarity(
        &PatternTokens::new(pattern, stopwords),
        &TextTokens::new(text),
    )
}

fn similarity(pattern: &PatternTokens, text: &TextTokens) -> f64 {
    let pattern_norm = &pattern.norm;
    let text_norm = &text.norm;
    let pattern_tokens: Vec<&str> = pattern.tokens.iter().map(String::as_str).collect();
    let text_tokens: Vec<&str> = text.tokens.iter().map(String::as_str).collect();

    if pattern_tokens.is_empty() {
        return 0.0;
    }

    // Count how many pattern tokens match (exact or substring)
    let mut matched_tokens = 0;
    let mut total_token_score = 0.0;
    let mut token_matched = vec![false; pattern_tokens.len()];

    for (token_idx, pt) in pattern_tokens.iter().enumerate() {
        // Check for exact token match first. Tokens are already canonical
        // (no punctuation, no diacritics).
        if text_tokens.contains(pt) {
            matched_tokens += 1;
            total_token_score += 1.0;
            token_matched[token_idx] = true;
            continue;
        }

        // Single-digit tokens (sequel numbers like "5") only get exact match,
        // never substring or fuzzy — skip to next token if no exact match found.
        if pt.len() < 2 {
            continue;
        }

        // Check for substring match in any token.
        // Text containing pattern token: require pattern token to be ≥50%
        // of text token length, preventing "galaxy" matching inside
        // "samhydetorrentgalaxy" (6/25 = 24%, rejected).
        // Pattern containing text token: require text token to be ≥75%
        // of pattern token length (existing guard against date fragments).
        if text_tokens.iter().any(|tt| {
            if tt.contains(pt) {
                return pt.len() * 100 >= tt.len() * 50;
            }
            if pt.contains(tt) {
                return tt.len() * 100 >= pt.len() * 75;
            }
            false
        }) {
            matched_tokens += 1;
            total_token_score += 0.8;
            token_matched[token_idx] = true;
            continue;
        }

        // Check for fuzzy match using Jaro-Winkler (threshold 0.90 to avoid
        // false positives like "brazil" ≈ "abril" which share characters but
        // are unrelated words).
        let (best_jw, best_jw_token) = text_tokens
            .iter()
            .map(|tt| (jaro_winkler(pt, tt), *tt))
            .max_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
            .unwrap_or((0.0, ""));

        if best_jw >= 0.90 {
            // Sanity check: tokens with identical letters but different digits
            // are structured identifier mismatches (e.g. s01e01 vs s01e07),
            // not legitimate typos. Reject these.
            if !jw_digit_sanity_check(pt, best_jw_token) {
                continue;
            }
            // Short pattern tokens (≤5 chars) get an extra edit-distance
            // guard: with the Winkler prefix boost, 4-char words like "beef"
            // can score JW ≈ 0.91 against "belief" despite an edit distance
            // of 2 (half the word). Require ED ≤ 1 for short tokens so only
            // plurals / single-char typos slip through.
            if pt.chars().count() <= 5 && levenshtein(pt, best_jw_token) > 1 {
                continue;
            }
            matched_tokens += 1;
            total_token_score += best_jw * 0.9;
            token_matched[token_idx] = true;
        }
    }

    // Calculate final score:
    // - 70% based on token matching ratio
    // - 30% based on average score of matched tokens
    let token_match_ratio = matched_tokens as f64 / pattern_tokens.len() as f64;
    let avg_token_score = if matched_tokens > 0 {
        total_token_score / matched_tokens as f64
    } else {
        0.0
    };

    // Also check if the full pattern appears as a substring (very high confidence).
    // Both sides are already canonical, so direct contains() is sufficient.
    let full_pattern_match = if text_norm.contains(pattern_norm.as_str()) {
        0.2
    } else {
        0.0
    };

    let mut base_score = token_match_ratio * 0.7 + avg_token_score * 0.3;
    let mut guard_capped = false;

    // Guard: if only purely-numeric tokens matched (e.g. "4" and "2026" but not
    // "terrifier"), cap the score. Numeric tokens alone are too common to be
    // meaningful — the title word must match for a real hit.
    if matched_tokens > 0 {
        let has_nontrivial_match = pattern_tokens
            .iter()
            .zip(&token_matched)
            .any(|(pt, &matched)| matched && !pt.chars().all(|c| c.is_ascii_digit()));
        if !has_nontrivial_match {
            base_score = base_score.min(0.5);
            guard_capped = true;
        }
    }

    // Guard: if pattern contains season/episode identifiers (s06, s01e03, 1x07),
    // require the majority of title words (non-identifier, non-numeric tokens) to match.
    // Without this, "Catch! Teenieping S06" matches "To Catch a Smuggler S06"
    // because "catch" (a common word) + "s06" match (2/3), but the distinguishing
    // title token "teenieping" doesn't match at all.
    if matched_tokens > 0 {
        let has_season_token = pattern_tokens.iter().any(|pt| is_season_episode_token(pt));
        if has_season_token {
            let title_words: Vec<(usize, &&str)> = pattern_tokens
                .iter()
                .enumerate()
                .filter(|(_, pt)| {
                    !is_season_episode_token(pt)
                        && !pt.chars().all(|c| c.is_ascii_digit())
                        && pt.len() >= 2
                })
                .collect();
            let title_word_count = title_words.len();
            let title_words_matched = title_words
                .iter()
                .filter(|(idx, _)| token_matched[*idx])
                .count();
            // If there are title words and fewer than half matched, cap the score.
            // This catches "Catch! Teenieping S06" vs "To Catch a Smuggler S06"
            // where only 1/2 title words match ("catch" but not "teenieping").
            if title_word_count > 0 && title_words_matched * 2 <= title_word_count {
                base_score = base_score.min(0.5);
                guard_capped = true;
            }

            // Guard: release show name must not have extra significant words
            // compared to the pattern show name. Prevents "Paradise S02" matching
            // "Hells Paradise S02E06" — the extra "Hells" means a different show.
            let is_show_name_word = |t: &str| -> bool {
                !(t.len() < 2
                    || t.chars().all(|c| c.is_ascii_digit())
                    || is_season_episode_token(t)
                    || t.starts_with('[')
                    || t.starts_with('('))
            };

            let pattern_show: Vec<&str> = pattern_tokens
                .iter()
                .take_while(|pt| !is_season_episode_token(pt))
                .filter(|pt| is_show_name_word(pt))
                .copied()
                .collect();

            let release_show: Vec<&str> = text_tokens
                .iter()
                .take_while(|tt| !is_season_episode_token(tt))
                .filter(|tt| is_show_name_word(tt))
                .copied()
                .collect();

            if !pattern_show.is_empty() {
                let extra = release_show
                    .iter()
                    .filter(|rw| !pattern_show.iter().any(|pw| pw == *rw))
                    .count();
                if extra > 0 {
                    base_score = base_score.min(0.5);
                    guard_capped = true;
                }
            }
        }
    }

    // Bonus for full pattern substring match — skip when a guard has capped
    // the score, since the substring match is misleading in those cases
    // (e.g. "Paradise S02" is a substring of "Hells Paradise S02E06").
    let bonus = if guard_capped {
        0.0
    } else {
        full_pattern_match
    };
    (base_score + bonus).min(1.0)
}

/// Levenshtein edit distance. Used as a safety net for short tokens where
/// Jaro-Winkler's prefix boost can turn a 2-edit mismatch ("beef" vs
/// "belief") into a 0.91 score.
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut curr = vec![0usize; b.len() + 1];
    for (i, ac) in a.iter().enumerate() {
        curr[0] = i + 1;
        for (j, bc) in b.iter().enumerate() {
            let cost = if ac == bc { 0 } else { 1 };
            curr[j + 1] = (curr[j] + 1).min(prev[j + 1] + 1).min(prev[j] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[b.len()]
}

/// Jaro-Winkler similarity algorithm
/// Returns a score between 0.0 and 1.0
fn jaro_winkler(s1: &str, s2: &str) -> f64 {
    if s1.is_empty() && s2.is_empty() {
        return 1.0;
    }
    if s1.is_empty() || s2.is_empty() {
        return 0.0;
    }

    let s1_chars: Vec<char> = s1.chars().collect();
    let s2_chars: Vec<char> = s2.chars().collect();

    let jaro = jaro_similarity(&s1_chars, &s2_chars);

    // Calculate common prefix length (up to 4 characters)
    let prefix_len = s1_chars
        .iter()
        .zip(s2_chars.iter())
        .take(4)
        .take_while(|(c1, c2)| c1 == c2)
        .count();

    // Winkler modification: boost for common prefix
    let winkler_boost = 0.1;
    jaro + (prefix_len as f64 * winkler_boost * (1.0 - jaro))
}

/// Jaro similarity (base for Jaro-Winkler)
fn jaro_similarity(s1: &[char], s2: &[char]) -> f64 {
    let len1 = s1.len();
    let len2 = s2.len();

    if len1 == 0 && len2 == 0 {
        return 1.0;
    }
    if len1 == 0 || len2 == 0 {
        return 0.0;
    }

    let match_distance = (len1.max(len2) / 2).saturating_sub(1);

    let mut s1_matches = vec![false; len1];
    let mut s2_matches = vec![false; len2];

    let mut matches = 0;
    let mut transpositions = 0;

    // Find matches
    for i in 0..len1 {
        let start = i.saturating_sub(match_distance);
        let end = (i + match_distance + 1).min(len2);

        for j in start..end {
            if s2_matches[j] || s1[i] != s2[j] {
                continue;
            }
            s1_matches[i] = true;
            s2_matches[j] = true;
            matches += 1;
            break;
        }
    }

    if matches == 0 {
        return 0.0;
    }

    // Count transpositions
    let mut k = 0;
    for i in 0..len1 {
        if !s1_matches[i] {
            continue;
        }
        while !s2_matches[k] {
            k += 1;
        }
        if s1[i] != s2[k] {
            transpositions += 1;
        }
        k += 1;
    }

    let m = matches as f64;
    let t = (transpositions / 2) as f64;

    (m / len1 as f64 + m / len2 as f64 + (m - t) / m) / 3.0
}

#[cfg(test)]
mod reference;

#[cfg(test)]
mod tests {
    use super::*;

    struct ScoreCase {
        label: &'static str,
        pattern: &'static str,
        text: &'static str,
        min: f64,
        max: f64,
    }

    fn run_score_cases(cases: &[ScoreCase]) {
        for c in cases {
            let score = fuzzy_score(c.pattern, c.text);
            if c.min >= 0.0 {
                assert!(
                    score >= c.min,
                    "[{}] score {:.3} < min {:.3}\n  pattern: {}\n  text: {}",
                    c.label,
                    score,
                    c.min,
                    c.pattern,
                    c.text
                );
            }
            if c.max >= 0.0 {
                assert!(
                    score < c.max,
                    "[{}] score {:.3} >= max {:.3}\n  pattern: {}\n  text: {}",
                    c.label,
                    score,
                    c.max,
                    c.pattern,
                    c.text
                );
            }
        }
    }

    #[test]
    fn test_match_any() {
        let result = match_any(
            &["brandon".to_string(), "sanderson".to_string()],
            "mistborn by brandon sanderson",
        );
        assert!(result.is_some());
        let r = result.unwrap();
        assert_eq!(r.score, 1.0);
        assert_eq!(r.matched_pattern, "brandon");
    }

    #[test]
    fn test_match_any_dot_separated_scene_names() {
        // Dot-separated scene titles must match space-separated patterns
        let result = match_any(
            &["Black Rabbit".to_string()],
            "black.rabbit.s01e01.1080p.nf.web-dl.ddp5.1.atmos.h.264-flux",
        );
        assert!(
            result.is_some(),
            "dot-separated title should match space-separated pattern"
        );

        let result = match_all(
            &["black".to_string(), "rabbit".to_string()],
            "black.rabbit.s01e01.the.cyclone.720p.nf.web-dl",
        );
        assert!(
            result.is_some(),
            "match_all should also work with dot-separated titles"
        );
    }

    #[test]
    fn test_match_any_apostrophe_variants() {
        // Scene releases handle apostrophes two ways:
        //   "Margo's" → "Margo.s" (treated as a separator, becomes a dot)
        //   "Margo's" → "Margos" (dropped entirely)
        // Both must match a pattern that contains the apostrophe.
        let dot_form = match_any(
            &["Margo's Got Money Troubles".to_string()],
            "margo.s.got.money.troubles.s01e01.1080p.atvp.web-dl.ddp5.1.atmos.h.264-bloom",
        );
        assert!(
            dot_form.is_some(),
            "pattern with apostrophe should match scene release where apostrophe became a dot"
        );

        let dropped_form = match_any(
            &["Margo's Got Money Troubles".to_string()],
            "margos.got.money.troubles.s01e01.1080p.atvp.web-dl.ddp5.1.atmos.h265-theblackking",
        );
        assert!(
            dropped_form.is_some(),
            "pattern with apostrophe should match scene release where apostrophe was dropped"
        );

        let spaced_dropped = match_any(
            &["Margo's Got Money Troubles".to_string()],
            "margos got money troubles s01e01 1080p atvp web-dl ddp5 1 atmos h 264-flux",
        );
        assert!(
            spaced_dropped.is_some(),
            "space-separated release with apostrophe dropped must also match"
        );

        // match_all should behave the same way
        let all_dropped = match_all(
            &["Margo's".to_string(), "Troubles".to_string()],
            "margos.got.money.troubles.s01e01.1080p.atvp.web-dl",
        );
        assert!(
            all_dropped.is_some(),
            "match_all with apostrophe pattern should match release with apostrophe dropped"
        );
    }

    #[test]
    fn test_match_any_hyphen_variants() {
        // Hyphenated show/movie names ("Spider-Man") appear in releases as:
        //   "Spider-Man" (hyphen preserved)
        //   "Spider.Man" (hyphen → dot → space)
        //   "SpiderMan"  (hyphen dropped entirely)
        // All three must match the pattern.
        let preserved = match_any(
            &["Spider-Man".to_string()],
            "spider-man.across.the.spider-verse.2023.2160p.web-dl.x265",
        );
        assert!(
            preserved.is_some(),
            "hyphen preserved in release must match"
        );

        let separator = match_any(
            &["Spider-Man".to_string()],
            "spider.man.no.way.home.2021.1080p.bluray.x264-group",
        );
        assert!(separator.is_some(), "hyphen → dot in release must match");

        let dropped = match_any(
            &["Spider-Man".to_string()],
            "spiderman.homecoming.2017.1080p.bluray.x264-group",
        );
        assert!(dropped.is_some(), "hyphen dropped in release must match");

        // Space-separated pattern vs hyphenated release
        let space_pattern = match_any(
            &["Spider Man".to_string()],
            "spider-man.homecoming.2017.1080p.bluray.x264-group",
        );
        assert!(
            space_pattern.is_some(),
            "space-separated pattern must match hyphenated release"
        );
    }

    #[test]
    fn test_match_any_smart_quote_apostrophe() {
        // Metadata sources (IMDB, TMDB) sometimes return curly/typographic quotes
        // instead of straight ASCII apostrophes. Both must match the same scene releases.
        let smart_quote_pattern = "Margo\u{2019}s Got Money Troubles";
        let result = match_any(
            &[smart_quote_pattern.to_string()],
            "margos.got.money.troubles.s01e01.1080p.atvp.web-dl",
        );
        assert!(
            result.is_some(),
            "smart-quote apostrophe pattern must match dropped-apostrophe release"
        );

        let result2 = match_any(
            &[smart_quote_pattern.to_string()],
            "margo.s.got.money.troubles.s01e01.1080p.atvp.web-dl",
        );
        assert!(
            result2.is_some(),
            "smart-quote apostrophe pattern must match separator-apostrophe release"
        );

        // Left single quote (U+2018) — less common but possible
        let left_quote_pattern = "\u{2018}Allo \u{2018}Allo";
        let result3 = match_any(
            &[left_quote_pattern.to_string()],
            "allo.allo.s01e01.1080p.web-dl",
        );
        assert!(result3.is_some(), "left-quote pattern must match");
    }

    #[test]
    fn test_match_any_diacritics() {
        // Scene releases strip accents. Patterns from metadata keep them.
        let result = match_any(
            &["Pokémon".to_string()],
            "pokemon.s01e01.indigo.league.1080p.web-dl",
        );
        assert!(
            result.is_some(),
            "accented pattern must match unaccented release"
        );

        // Reverse direction: pattern has no accent, release has accent (rare but real)
        let result2 = match_any(
            &["Pokemon".to_string()],
            "pokémon.s01e01.indigo.league.1080p.web-dl",
        );
        assert!(
            result2.is_some(),
            "unaccented pattern must match accented release"
        );

        // Multiple accented chars
        let result3 = match_any(
            &["Café Con Aroma de Mujer".to_string()],
            "cafe.con.aroma.de.mujer.s01e01.1080p.web-dl",
        );
        assert!(
            result3.is_some(),
            "multi-accent pattern must match stripped release"
        );
    }

    #[test]
    fn test_match_any_underscore_separator() {
        // Scene releases (especially music) sometimes use underscores instead of dots.
        let result = match_any(
            &["Radiohead OK Computer".to_string()],
            "radiohead_ok_computer_1997_flac",
        );
        assert!(
            result.is_some(),
            "space-separated pattern must match underscore-separated release"
        );
    }

    #[test]
    fn test_match_any_ampersand_vs_and() {
        // "Law & Order" pattern must match releases that use "and" instead of "&"
        let result = match_any(
            &["Law & Order".to_string()],
            "law.and.order.s08e09.1080p.web-dl",
        );
        assert!(
            result.is_some(),
            "pattern with & should match release with 'and'"
        );

        // Release that keeps the & should also match
        let result = match_any(&["Law & Order".to_string()], "law.&.order.s08e09.720p.hdtv");
        assert!(
            result.is_some(),
            "pattern with & should match release that also has &"
        );

        // Pattern with "and" should match release with &
        let result = match_any(
            &["Law and Order".to_string()],
            "law.&.order.s08e09.720p.hdtv",
        );
        assert!(
            result.is_some(),
            "pattern with 'and' should match release with &"
        );
    }

    #[test]
    fn test_match_all_ampersand_vs_and() {
        let result = match_all(
            &["law & order".to_string(), "1080p".to_string()],
            "law.and.order.s08e09.1080p.web-dl",
        );
        assert!(
            result.is_some(),
            "match_all with & pattern should match release with 'and'"
        );
    }

    #[test]
    fn test_fuzzy_score_ampersand_vs_and() {
        // "Law & Order" should score well against "Law and Order" release
        let score = fuzzy_score("law & order", "law.and.order.s08e09.1080p.web-dl");
        assert!(
            score > 0.7,
            "Score {} should be > 0.7 for & vs 'and'",
            score
        );
    }

    #[test]
    fn test_match_any_no_match() {
        let result = match_any(
            &["tolkien".to_string(), "hobbit".to_string()],
            "mistborn by brandon sanderson",
        );
        assert!(result.is_none());
    }

    #[test]
    fn test_match_all() {
        let result = match_all(
            &["brandon".to_string(), "sanderson".to_string()],
            "mistborn by brandon sanderson",
        );
        assert!(result.is_some());
        let r = result.unwrap();
        assert_eq!(r.score, 1.0);
    }

    #[test]
    fn test_match_all_partial() {
        let result = match_all(
            &["brandon".to_string(), "tolkien".to_string()],
            "mistborn by brandon sanderson",
        );
        assert!(result.is_none());
    }

    #[test]
    fn test_fuzzy_score_exact() {
        let score = fuzzy_score(
            "brandon sanderson mistborn",
            "mistborn by brandon sanderson",
        );
        assert!(score > 0.8, "Score {} should be > 0.8", score);
    }

    #[test]
    fn test_fuzzy_score_partial() {
        let score = fuzzy_score(
            "sanderson mistborn",
            "mistborn final empire brandon sanderson epub",
        );
        assert!(score > 0.7, "Score {} should be > 0.7", score);
    }

    #[test]
    fn test_fuzzy_score_no_match() {
        let score = fuzzy_score("harry potter rowling", "mistborn by brandon sanderson");
        assert!(score < 0.5, "Score {} should be < 0.5", score);
    }

    #[test]
    fn test_jaro_winkler_identical() {
        let score = jaro_winkler("sanderson", "sanderson");
        assert_eq!(score, 1.0);
    }

    #[test]
    fn test_jaro_winkler_similar() {
        let score = jaro_winkler("sanderson", "sandersen");
        assert!(score > 0.9, "Score {} should be > 0.9", score);
    }

    #[test]
    fn test_jaro_winkler_different() {
        let score = jaro_winkler("sanderson", "tolkien");
        assert!(score < 0.6, "Score {} should be < 0.6", score);
    }

    #[test]
    fn test_short_title_does_not_fuzzy_match_longer_word() {
        // Regression: "BEEF" (4 chars) vs "belief" inside a release name
        // scores JW ≈ 0.91 purely because of the 2-char "be" Winkler prefix
        // boost, even though the edit distance is 2 (half the word).
        // That slipped past the 0.70 koblem-review overlap gate and forwarded
        // a completely unrelated show to the LLM.
        let score = fuzzy_score(
            "BEEF",
            "Before.2024.S01E07.The.Power.of.Belief.REPACK.2160p.ATVP.WEB-DL.DDP5.1.Atmos.DV.HDR10Plus.H.265-Kitsune",
        );
        assert!(
            score < 0.70,
            "BEEF vs a Belief release must stay below the 0.70 overlap gate (got {:.3})",
            score
        );
    }

    #[test]
    fn test_short_title_still_matches_single_edit_variants() {
        // Positive regression: genuine near-matches (edit distance 1) must
        // still clear the gate — plurals, single-char typos, etc.
        let plural = fuzzy_score("beef", "beefs.2024.s01e01.1080p.web-dl-group");
        assert!(
            plural >= 0.70,
            "'beef' should still match 'beefs' (ED=1), got {:.3}",
            plural
        );
        let typo = fuzzy_score("dune", "dunes.part.two.2024.2160p.web-dl");
        assert!(
            typo >= 0.70,
            "'dune' should still match 'dunes' (ED=1), got {:.3}",
            typo
        );
    }

    #[test]
    fn test_match_patterns_isbn() {
        // ISBN should match exactly
        let result = match_patterns(
            &["9780765311788".to_string()],
            "any",
            0.75,
            "brandon sanderson mistborn 9780765311788 epub",
        );
        assert!(result.is_some());
        assert_eq!(result.unwrap().score, 1.0);
    }

    #[test]
    fn test_fuzzy_alternative_patterns() {
        // Patterns are alternatives - should match best one, not combine
        let result = match_fuzzy(
            &[
                "will wight blackflame".to_string(),
                "blackflame".to_string(),
            ],
            "will wight - [cradle 03] - blackflame (retail) (epub)",
            0.65,
        );
        assert!(result.is_some());
        let r = result.unwrap();
        // Should match "will wight blackflame", not "will wight blackflame blackflame"
        assert!(
            !r.matched_pattern.contains("blackflame blackflame"),
            "Matched pattern '{}' should not contain duplicate words",
            r.matched_pattern
        );
        assert!(r.score > 0.8, "Score {} should be > 0.8", r.score);
    }

    #[test]
    fn test_fuzzy_score_unrelated_content_is_low() {
        // Regression test: TV show titles should not match ebook searches
        // "St Denis Medical" should score very low against "Oathbringer Brandon Sanderson"
        let score = fuzzy_score(
            "oathbringer brandon sanderson",
            "st denis medical s01e05 the clintons 1080p amzn web-dl ddp5 1 h 264-ntb",
        );
        assert!(
            score < 0.5,
            "Score {} should be < 0.5 for completely unrelated content",
            score
        );

        // Also test with another TV show that was downloaded
        let score2 = fuzzy_score(
            "oathbringer brandon sanderson",
            "the pitt s01e01 cold open 2160p max web-dl ddp5 1 atmos dv hdr h 265-ntb",
        );
        assert!(
            score2 < 0.5,
            "Score {} should be < 0.5 for completely unrelated content",
            score2
        );
    }

    #[test]
    fn test_fuzzy_score_similar_content_is_high() {
        // Actual book match should score high
        let score = fuzzy_score(
            "oathbringer brandon sanderson",
            "brandon sanderson - stormlight archive 03 - oathbringer (epub)",
        );
        assert!(
            score >= 0.75,
            "Score {} should be >= 0.75 for actual book match",
            score
        );
    }

    #[test]
    fn test_fuzzy_score_date_fragment_false_positive() {
        // Regression: "BrokenLatinaWhores 26 02 25 Abril" matched
        // "MotoGP 2026 R02 Brazil" at 0.659 because:
        // - "2026" contained date fragment "26" (substring match)
        // - "r02" contained date fragment "02" (substring match)
        // - "brazil" ≈ "abril" via Jaro-Winkler (0.878)
        // All three were false positives.
        let score = fuzzy_score(
            "motogp 2026 r02 brazil race",
            "brokenlatinawhores 26 02 25 abril xxx 1080p mp4-fetish",
        );
        assert!(
            score < 0.5,
            "Score {} should be < 0.5 for date-fragment false positive",
            score
        );

        // Also test the qualifying/sprint variants
        let score2 = fuzzy_score(
            "motogp 2026 r02 brazil qualifying",
            "brokenlatinawhores 26 02 25 abril xxx 1080p mp4-fetish",
        );
        assert!(
            score2 < 0.5,
            "Score {} should be < 0.5 for qualifying variant",
            score2
        );
    }

    #[test]
    fn test_fuzzy_score_sport_vs_adult_content() {
        // Sport event patterns must not match adult content
        let sport_patterns = [
            "motogp 2025 r01 thailand race",
            "motogp 2025 round 1 thailand race",
            "motogp 2025 r01 thailand",
            "motogp r01 thailand race",
            "formula 1 2026 r03 australia race",
            "f1 2026 r01 bahrain qualifying",
        ];
        let adult_titles = [
            "broken latina whores xxx 720p webrip",
            "latina teen casting couch 1080p",
            "big beautiful women vol 3 xxx dvdrip",
        ];

        for pattern in &sport_patterns {
            for title in &adult_titles {
                let score = fuzzy_score(pattern, title);
                assert!(
                    score < 0.5,
                    "Sport pattern '{}' vs adult '{}' scored {} (should be < 0.5)",
                    pattern,
                    title,
                    score
                );
            }
        }
    }

    #[test]
    fn test_fuzzy_score_short_show_names_preserved() {
        // 2-char show names like "24" must still contribute to scoring
        // so they penalise mismatches against other shows
        let correct = fuzzy_score("24 s01e05", "24 s01e05 720p web-dl");
        let wrong_show = fuzzy_score("24 s01e05", "other show s01e05 720p");
        assert!(
            correct > wrong_show,
            "Correct match ({}) should score higher than wrong show ({})",
            correct,
            wrong_show
        );
        assert!(correct > 0.8, "Correct match {} should be high", correct);
    }

    #[test]
    fn test_fuzzy_score_substring_direction() {
        // Text containing pattern token should always match (strong signal)
        let score = fuzzy_score("s01e05", "show.name.s01e05.720p.web-dl");
        assert!(
            score > 0.7,
            "Pattern inside compound text token should match: {}",
            score
        );

        // Pattern containing short text fragment should NOT match easily
        // "2026" containing date fragment "26" is weak signal
        let score2 = fuzzy_score("2026", "26 02 25");
        assert!(
            score2 < 0.8,
            "Pattern containing short text fragment should not score high: {}",
            score2
        );
    }

    // ── Positive matching: TV ──────────────────────────────────────────

    #[test]
    fn test_positive_matching_tv() {
        run_score_cases(&[
            ScoreCase {
                label: "tv space-separated",
                pattern: "show name s01e05",
                text: "show name s01e05 1080p web-dl x264-group",
                min: 0.75,
                max: -1.0,
            },
            ScoreCase {
                label: "tv dot-separated",
                pattern: "show name s01e05",
                text: "show.name.s01e05.1080p.web-dl.x264-group",
                min: 0.65,
                max: -1.0,
            },
            ScoreCase {
                label: "tv multi-episode",
                pattern: "show name s01e05",
                text: "show name s01e05-e07 720p web h264-group",
                min: 0.75,
                max: -1.0,
            },
            ScoreCase {
                label: "tv complete season",
                pattern: "breaking bad s03",
                text: "breaking bad s03 complete 1080p bluray-group",
                min: 0.75,
                max: -1.0,
            },
            ScoreCase {
                label: "tv season word",
                pattern: "breaking bad season 3",
                text: "breaking bad season 3 complete 1080p bluray",
                min: 0.75,
                max: -1.0,
            },
            ScoreCase {
                label: "tv daily show",
                pattern: "the daily show 2024-01-15",
                text: "the daily show 2024 01 15 720p hdtv-group",
                min: 0.65,
                max: -1.0,
            },
        ]);
    }

    // ── Positive matching: Movies ──────────────────────────────────────

    #[test]
    fn test_positive_matching_movies() {
        run_score_cases(&[
            ScoreCase {
                label: "movie standard",
                pattern: "dune part two 2024",
                text: "dune part two 2024 1080p bluray x264-group",
                min: 0.75,
                max: -1.0,
            },
            ScoreCase {
                label: "movie directors cut",
                pattern: "dune part two 2024",
                text: "dune part two 2024 directors cut 1080p bluray-group",
                min: 0.75,
                max: -1.0,
            },
        ]);
    }

    // ── Positive matching: Books ───────────────────────────────────────

    #[test]
    fn test_positive_matching_books() {
        run_score_cases(&[
            ScoreCase {
                label: "book retail epub",
                pattern: "mistborn brandon sanderson",
                text: "brandon sanderson - mistborn (retail) (epub)",
                min: 0.75,
                max: -1.0,
            },
            ScoreCase {
                label: "book series numbering",
                pattern: "brandon sanderson mistborn",
                text: "brandon sanderson - [cosmere 01] - mistborn (retail) (epub)",
                min: 0.75,
                max: -1.0,
            },
            ScoreCase {
                label: "audiobook m4b",
                pattern: "mistborn brandon sanderson",
                text: "sanderson brandon - mistborn 01 - the final empire [m4b]",
                min: 0.65,
                max: -1.0,
            },
        ]);
    }

    // ── Positive matching: Music ───────────────────────────────────────

    #[test]
    fn test_positive_matching_music() {
        run_score_cases(&[
            ScoreCase {
                label: "music space-separated",
                pattern: "radiohead ok computer",
                text: "radiohead ok computer 1997 flac",
                min: 0.75,
                max: -1.0,
            },
            ScoreCase {
                label: "music hyphen-separated",
                pattern: "radiohead ok computer",
                text: "radiohead-ok_computer-1997-flac-group",
                min: 0.65,
                max: -1.0,
            },
        ]);
    }

    // ── Positive matching: Sport (score_against_targets) ───────────────

    #[test]
    fn test_positive_matching_sport() {
        let targets = vec![
            MatchTarget {
                id: "primary".to_string(),
                target: "MotoGP 2026 R02 Brazil Race".to_string(),
                min_score: None,
                semantic_target: None,
            },
            MatchTarget {
                id: "broad".to_string(),
                target: "MotoGP 2026 R02 Brazil".to_string(),
                min_score: Some(0.85),
                semantic_target: None,
            },
        ];

        // Exact match against primary target
        let result = score_against_targets(
            "motogp 2026 r02 brazil grand prix race 1080p web-dl",
            &targets,
            0.65,
        );
        assert!(result.is_some(), "Primary sport target should match");
        let r = result.unwrap();
        assert!(r.score >= 0.65, "Score {} should be >= 0.65", r.score);

        // F1 targets
        let f1_targets = vec![MatchTarget {
            id: "f1-primary".to_string(),
            target: "Formula 1 2026 R03 Australia Qualifying".to_string(),
            min_score: None,
            semantic_target: None,
        }];
        let result2 = score_against_targets(
            "formula 1 2026 r03 australia qualifying 1080p",
            &f1_targets,
            0.65,
        );
        assert!(result2.is_some(), "F1 target should match");

        // Broad target with higher threshold for sprint variant
        let result3 = score_against_targets("motogp 2026 r02 brazil sprint 720p", &targets, 0.65);
        assert!(result3.is_some(), "Broad sport target should match sprint");

        // Different sport with same round/year: the broad target (0.85) rejects it,
        // but the primary target with default 0.65 may accept it because they
        // share "2026", "r02", "race". In practice, Prowlarr keyword search
        // filters by league name, and category filtering prevents cross-category.
        // Here we verify the broad target does its job.
        let broad_only = vec![MatchTarget {
            id: "broad".to_string(),
            target: "MotoGP 2026 R02 Brazil".to_string(),
            min_score: Some(0.85),
            semantic_target: None,
        }];
        let result4 = score_against_targets(
            "formula 1 2026 r02 saudi arabia race 1080p",
            &broad_only,
            0.85,
        );
        assert!(
            result4.is_none(),
            "Broad target (0.85) should reject different sport (got {:?})",
            result4.map(|r| r.score)
        );
    }

    // ── Positive matching: Games, Comics, Software ─────────────────────

    #[test]
    fn test_positive_matching_games_comics_software() {
        run_score_cases(&[
            ScoreCase {
                label: "game scene release",
                pattern: "elden ring pc",
                text: "elden ring v1 2 3 codex",
                min: 0.65,
                max: -1.0,
            },
            ScoreCase {
                label: "comic digital",
                pattern: "saga 054",
                text: "saga 054 2024 digital empire cbz",
                min: 0.65,
                max: -1.0,
            },
            ScoreCase {
                label: "software release",
                pattern: "photoshop 2024",
                text: "adobe photoshop 2024 v25 1 windows",
                min: 0.65,
                max: -1.0,
            },
        ]);
    }

    // ── Cross-type false positives ─────────────────────────────────────

    #[test]
    fn test_cross_type_false_positives() {
        run_score_cases(&[
            // Sport vs adult (the original bug)
            ScoreCase {
                label: "sport vs adult (original bug)",
                pattern: "motogp 2026 r02 brazil race",
                text: "brokenlatinawhores 26 02 25 abril xxx 1080p mp4-fetish",
                min: -1.0,
                max: 0.5,
            },
            ScoreCase {
                label: "f1 vs adult (location word collision)",
                pattern: "formula 1 2026 r03 australia qualifying",
                text: "australian amateur teens vol 3 xxx 720p",
                min: -1.0,
                max: 0.6,
            },
            // TV vs movie
            ScoreCase {
                label: "tv vs movie (word 'bad')",
                pattern: "breaking bad s03",
                text: "bad boys 2024 1080p bluray x264-group",
                min: -1.0,
                max: 0.6,
            },
            ScoreCase {
                label: "tv vs movie (word 'show')",
                pattern: "show name s01e05",
                text: "the show 2024 1080p bluray x264-group",
                min: -1.0,
                max: 0.6,
            },
            // Book vs TV
            ScoreCase {
                label: "book vs tv (first name collision)",
                pattern: "mistborn brandon sanderson",
                text: "brandon s01e03 720p web-dl x264-group",
                min: -1.0,
                max: 0.6,
            },
            // Music vs movie
            ScoreCase {
                label: "music vs movie (word 'computer')",
                pattern: "radiohead ok computer",
                text: "the computer 2024 1080p bluray",
                min: -1.0,
                max: 0.6,
            },
            // TV: show name is proper subset of different show
            ScoreCase {
                label: "paradise vs hells paradise (subset show name)",
                pattern: "paradise s02",
                text: "[yameii] hells paradise-s02e06 [english dub] [cr web-dl 1080p h264 aac] [c5b02ba9]",
                min: -1.0,
                max: 0.55,
            },
            ScoreCase {
                label: "paradise vs hells paradise dot-separated",
                pattern: "paradise s02",
                text: "hells.paradise.s02e08.chrysanthemum.and.peach.1080p.cr.web-dl.ddp2.0.h.264-kitsune",
                min: -1.0,
                max: 0.55,
            },
            // Correct Paradise match must still work
            ScoreCase {
                label: "paradise correct match",
                pattern: "paradise s02",
                text: "paradise.2025.s02e03.720p.web.h264-insidious",
                min: 0.75,
                max: -1.0,
            },
            ScoreCase {
                label: "paradise correct match with year in parens",
                pattern: "paradise s02",
                text: "paradise (2025) s02e05 (1080p dsnp web-dl h265 sdr ddp 5.1 english - hone)",
                min: 0.75,
                max: -1.0,
            },
            // Sport vs TV
            ScoreCase {
                label: "sport vs tv ('race'+'02'+'26')",
                pattern: "motogp 2026 r02 brazil race",
                text: "the amazing race s02e26 720p web-dl",
                min: -1.0,
                max: 0.6,
            },
            ScoreCase {
                label: "sport vs tv (word 'qualifying')",
                pattern: "formula 1 2026 r03 australia qualifying",
                text: "the qualifying round s03e01 720p hdtv",
                min: -1.0,
                max: 0.6,
            },
            // Game vs movie
            ScoreCase {
                label: "game vs movie (word 'ring')",
                pattern: "elden ring pc",
                text: "the lord of the rings 2024 1080p bluray",
                min: -1.0,
                max: 0.6,
            },
            // Sport vs different sport — shares "2026", "r02", "race" so raw score
            // is moderate (~0.72), but real sport match targets use min_score 0.85
            // for the broad target which rejects it. Tested below via score_against_targets.
            ScoreCase {
                label: "motogp vs f1 (same round/year, raw score)",
                pattern: "motogp 2026 r02 brazil race",
                text: "formula 1 2026 r02 saudi arabia race 1080p",
                min: -1.0,
                max: 0.75,
            },
        ]);
    }

    // ── Numeric-only match guard ──────────────────────────────────────

    #[test]
    fn test_numeric_only_match_capped() {
        run_score_cases(&[
            // Regression: "Terrifier 4 2026" matched BOOKWARE release at 0.77
            // because "4" and "2026" matched exactly (2/3 ratio).
            // The title word "terrifier" didn't match at all.
            ScoreCase {
                label: "movie vs bookware (only numbers match)",
                pattern: "terrifier 4 2026",
                text: "pluralsight first look anthropic claude opus 4 6 and sonnet 4 6 2026 bookware-geth",
                min: -1.0,
                max: 0.55,
            },
            // Numbers-only pattern still works when title also matches
            ScoreCase {
                label: "movie with matching title (numbers + title)",
                pattern: "terrifier 4 2026",
                text: "terrifier 4 2026 1080p bluray x264-group",
                min: 0.75,
                max: -1.0,
            },
            // Pure year match shouldn't score high
            ScoreCase {
                label: "year-only match capped",
                pattern: "some movie 2026",
                text: "completely different 2026 bluray",
                min: -1.0,
                max: 0.55,
            },
        ]);
    }

    // ── Numeric collision edge cases ───────────────────────────────────

    #[test]
    fn test_numeric_collision_edge_cases() {
        run_score_cases(&[
            ScoreCase {
                label: "year vs date fragment",
                pattern: "show 2026",
                text: "show 26 02 25 recap",
                min: -1.0,
                max: 0.65,
            },
            ScoreCase {
                label: "round vs date",
                pattern: "league r02 spain",
                text: "02 25 compilation",
                min: -1.0,
                max: 0.5,
            },
            ScoreCase {
                label: "episode vs year substring",
                pattern: "show s01e25",
                text: "show 2025 1080p",
                min: -1.0,
                max: 0.65,
            },
            ScoreCase {
                label: "issue number vs year",
                pattern: "saga 054",
                text: "2054 documentary 720p",
                min: -1.0,
                max: 0.65,
            },
            ScoreCase {
                label: "short show '24' vs year",
                pattern: "24 s01e05",
                text: "show 2024 1080p",
                min: -1.0,
                max: 0.65,
            },
            ScoreCase {
                label: "wrong round same league",
                pattern: "motogp 2026 r10 spain",
                text: "motogp 2026 r01 bahrain race",
                min: -1.0,
                max: 0.65,
            },
        ]);
    }

    // ── JW digit sanity check — episode/season/year mismatches ─────────

    #[test]
    fn test_jw_digit_sanity_rejects_episode_mismatch() {
        run_score_cases(&[
            // Score drops from 0.988 → 0.825 because the episode token is now
            // correctly rejected by the digit sanity check. The remaining 3/4
            // tokens still match ("dtf", "st", "louis"). Fixes 0b/0c provide
            // defense-in-depth by validating episode_key from the release title.
            ScoreCase {
                label: "different episode number (JW rejected, score reduced)",
                pattern: "dtf st louis s01e07",
                text: "dtf st louis s01e01 cornhole 1080p amzn web-dl ddp5 1 atmos h 264-flux",
                min: -1.0,
                max: 0.99, // was 0.988 before fix
            },
            ScoreCase {
                label: "different season number",
                pattern: "show s02e01",
                text: "show s01e01 720p web-dl",
                min: -1.0,
                max: 0.85,
            },
            ScoreCase {
                label: "1x format episode mismatch",
                pattern: "show 1x07",
                text: "show 1x01 720p web-dl",
                min: -1.0,
                max: 0.85,
            },
            // Note: "show season 2" — single-digit "2" is filtered out (len < 2),
            // so this effectively matches "show season" which is correct.
            // The compact format "show s02" properly distinguishes seasons.
            ScoreCase {
                label: "compact season format mismatch",
                pattern: "show s02",
                text: "show s01 complete 720p",
                min: -1.0,
                max: 0.85,
            },
        ]);
    }

    #[test]
    fn test_jw_digit_sanity_allows_correct_matches() {
        run_score_cases(&[
            ScoreCase {
                label: "same episode must match",
                pattern: "dtf st louis s01e01",
                text: "dtf st louis s01e01 cornhole 1080p",
                min: 0.75,
                max: -1.0,
            },
            ScoreCase {
                label: "legit author typo still matches",
                pattern: "brandon sandersen mistborn",
                text: "brandon sanderson mistborn retail epub",
                min: 0.75,
                max: -1.0,
            },
        ]);
    }

    #[test]
    fn test_jw_digit_sanity_check_unit() {
        // Should reject: same letters, different digits
        assert!(!jw_digit_sanity_check("s01e01", "s01e07"));
        assert!(!jw_digit_sanity_check("s02e01", "s01e01"));
        assert!(!jw_digit_sanity_check("x264", "x265"));
        assert!(!jw_digit_sanity_check("1x01", "1x07"));
        assert!(!jw_digit_sanity_check("2024", "2025"));
        assert!(!jw_digit_sanity_check("season1", "season2"));

        // Should allow: different letters (legitimate typos)
        assert!(jw_digit_sanity_check("sanderson", "sandersen"));
        assert!(jw_digit_sanity_check("mistborn", "misborn"));

        // Should allow: same token
        assert!(jw_digit_sanity_check("s01e01", "s01e01"));
        assert!(jw_digit_sanity_check("2024", "2024"));

        // Should allow: no digits at all
        assert!(jw_digit_sanity_check("test", "tset"));

        // Should allow: different letters even with digits (720p vs 720i)
        assert!(jw_digit_sanity_check("720p", "720i"));
    }

    // ── Dot-separated cross-sport false positives ──────────────────────

    #[test]
    fn test_fuzzy_score_dot_separated_cross_sport() {
        // Regression: MotoGP pattern matched FormulaE release because
        // dot-separated text was one giant token, and shared words
        // (2026, Brazil, Qualifying) matched as substrings.
        run_score_cases(&[
            ScoreCase {
                label: "motogp vs formulae (dot-separated)",
                pattern: "motogp 2026 r02 brazil qualifying 1",
                text: "formulae.2026.round01.brazil.qualifying.stan.web-dl.1080p.h264.english-mwr",
                min: -1.0,
                max: 0.75,
            },
            ScoreCase {
                label: "motogp vs formulae race (dot-separated)",
                pattern: "motogp 2026 r02 brazil race",
                text: "formulae.2026.round01.brazil.race.stan.web-dl.1080p.h264.english-mwr",
                min: -1.0,
                max: 0.75,
            },
            ScoreCase {
                label: "correct motogp match (dot-separated)",
                pattern: "motogp 2026 r02 brazil qualifying",
                text: "motogp.2026.r02.brazil.qualifying.1080p.web-dl",
                min: 0.75,
                max: -1.0,
            },
            ScoreCase {
                label: "correct motogp match (space-separated)",
                pattern: "motogp 2026 r02 brazil qualifying",
                text: "motogp 2026 r02 brazil qualifying 1080p web-dl",
                min: 0.75,
                max: -1.0,
            },
        ]);
    }

    // ── Jaro-Winkler edge cases ────────────────────────────────────────

    #[test]
    fn test_jaro_winkler_edge_cases() {
        // Pairs that should NOT pass 0.90 threshold
        let should_not_match = [("brazil", "abril"), ("sprint", "spirit"), ("race", "rice")];
        for (a, b) in &should_not_match {
            let jw = jaro_winkler(a, b);
            assert!(
                jw < 0.90,
                "JW('{}', '{}') = {:.3} should be < 0.90",
                a,
                b,
                jw
            );
        }

        // Pairs that should pass 0.90 threshold (typos/dropped letters)
        let should_match = [
            ("sanderson", "sandersen"),
            ("mistborn", "misborn"),
            ("thailand", "thailannd"),
        ];
        for (a, b) in &should_match {
            let jw = jaro_winkler(a, b);
            assert!(
                jw >= 0.90,
                "JW('{}', '{}') = {:.3} should be >= 0.90",
                a,
                b,
                jw
            );
        }
    }

    // ── Original false positives (must score below threshold) ─────────

    #[test]
    fn test_original_false_positives() {
        run_score_cases(&[
            ScoreCase {
                label: "toy story wrong year+sequel",
                pattern: "toy story 5 2026",
                text: "toy.story.1995.repack.720p.bluray.dd+5.1.x264-playhd",
                min: -1.0,
                max: 0.75,
            },
            // This still scores ~0.77 at the fuzzy level (5/7 tokens match)
            // but structural_reject() catches it via year mismatch (2023 vs 2026).
            ScoreCase {
                label: "mario galaxy vs bros group name",
                pattern: "the super mario galaxy movie 2026",
                text: "the.super.mario.bros.movie.2023.2160p.remux.hevc.hdr10.truehd.7.1-samhydetorrentgalaxy",
                min: -1.0,
                max: 0.80,
            },
        ]);
    }

    // ── Correct matches still work ────────────────────────────────────

    #[test]
    fn test_correct_matches_still_work() {
        run_score_cases(&[
            ScoreCase {
                label: "toy story 5 correct release",
                pattern: "toy story 5 2026",
                text: "toy.story.5.2026.1080p.bluray.x264-group",
                min: 0.75,
                max: -1.0,
            },
            ScoreCase {
                label: "movie with correct year",
                pattern: "movie title 2024",
                text: "movie.title.2024.1080p.bluray.x264-group",
                min: 0.75,
                max: -1.0,
            },
        ]);
    }

    // ── Group name pollution ──────────────────────────────────────────

    #[test]
    fn test_group_name_pollution() {
        run_score_cases(&[
            ScoreCase {
                label: "group name contains galaxy (title also has it)",
                pattern: "guardians of the galaxy 2023",
                text: "guardians.of.the.galaxy.vol.3.2023.1080p.bluray-samhydetorrentgalaxy",
                min: 0.75,
                max: -1.0,
            },
            ScoreCase {
                label: "group name only source of match word",
                pattern: "galaxy quest 1999",
                text: "some.other.movie.1999.1080p.bluray-intergalaxy",
                min: -1.0,
                max: 0.6,
            },
            ScoreCase {
                label: "long group name doesn't prevent title match",
                pattern: "the matrix 1999",
                text: "the.matrix.1999.1080p.bluray.x264-deathmatrixrebornreloaded",
                min: 0.75,
                max: -1.0,
            },
            ScoreCase {
                label: "group name doesn't pollute score (galaxy quest)",
                pattern: "galaxy quest 1999",
                text: "galaxy.quest.1999.1080p.bluray-samhydetorrentgalaxy",
                min: 0.75,
                max: -1.0,
            },
        ]);
    }

    // ── Sequel/number confusion ───────────────────────────────────────

    #[test]
    fn test_sequel_number_confusion() {
        run_score_cases(&[
            ScoreCase {
                label: "sequel number matches",
                pattern: "toy story 5 2026",
                text: "toy.story.5.2026.1080p.bluray.x264-group",
                min: 0.75,
                max: -1.0,
            },
            ScoreCase {
                label: "sequel number wrong",
                pattern: "toy story 5 2026",
                text: "toy.story.4.2019.1080p.bluray.x264-group",
                min: -1.0,
                max: 0.75,
            },
            ScoreCase {
                label: "number in title not sequel (2001)",
                pattern: "2001 a space odyssey 1968",
                text: "2001.a.space.odyssey.1968.1080p.bluray.x264-group",
                min: 0.65,
                max: -1.0,
            },
        ]);
    }

    // ── Year confusion with quality/codec numbers ─────────────────────

    #[test]
    fn test_year_vs_quality_numbers() {
        run_score_cases(&[
            ScoreCase {
                label: "x264 not a year",
                pattern: "movie 2024",
                text: "movie.2024.1080p.x264-group",
                min: 0.75,
                max: -1.0,
            },
            ScoreCase {
                label: "2160p not a year",
                pattern: "movie 2024",
                text: "movie.2024.2160p.uhd.bluray-group",
                min: 0.75,
                max: -1.0,
            },
        ]);
    }

    // ── Substring traps ──────────────────────────────────────────────

    #[test]
    fn test_substring_traps() {
        run_score_cases(&[
            ScoreCase {
                label: "ice not in police",
                pattern: "ice age 2002",
                text: "police.academy.1984.1080p.bluray-group",
                min: -1.0,
                max: 0.5,
            },
            ScoreCase {
                label: "ant not in enchanted",
                pattern: "ant man 2015",
                text: "enchanted.2007.1080p.bluray-group",
                min: -1.0,
                max: 0.5,
            },
            ScoreCase {
                label: "the not overpowered",
                pattern: "the flash 2023",
                text: "the.notebook.2004.1080p.bluray-group",
                min: -1.0,
                max: 0.6,
            },
            ScoreCase {
                label: "short common title (up vs setup)",
                pattern: "up 2009",
                text: "the.setup.2024.1080p.web-dl-group",
                min: -1.0,
                max: 0.6,
            },
        ]);
    }

    // ── Real scene release format stress tests ────────────────────────

    #[test]
    fn test_scene_release_stress() {
        // Note: fuzzy_score() receives pre-lowercased input in production
        // (match_fuzzy lowercases both), so tests must use lowercase text.
        run_score_cases(&[
            ScoreCase {
                label: "scene movie standard",
                pattern: "oppenheimer 2023",
                text: "oppenheimer.2023.1080p.bluray.remux.avc.dts-hd.ma.5.1-fgt",
                min: 0.75,
                max: -1.0,
            },
            ScoreCase {
                label: "scene tv with ep title",
                pattern: "show name s01e05",
                text: "show.name.s01e05.episode.title.1080p.amzn.web-dl.ddp5.1.h.264-ntb",
                min: 0.75,
                max: -1.0,
            },
            ScoreCase {
                label: "scene book retail",
                pattern: "mistborn brandon sanderson",
                text: "brandon.sanderson.-.mistborn.the.final.empire.(retail).(epub)",
                min: 0.75,
                max: -1.0,
            },
            ScoreCase {
                label: "scene sport multi quality",
                pattern: "motogp 2026 r02 brazil race",
                text: "motogp.2026.r02.brazil.race.1080p.web-dl.aac2.0.h.264-mwr",
                min: 0.75,
                max: -1.0,
            },
            ScoreCase {
                label: "brackets and special chars",
                pattern: "spider-man across the spider-verse 2023",
                text: "spider-man.across.the.spider-verse.2023.2160p.web-dl.ddp5.1.atmos.dv.x265-flux",
                min: 0.65,
                max: -1.0,
            },
            ScoreCase {
                label: "dual audio language tags don't confuse",
                pattern: "the french dispatch 2021",
                text: "the.french.dispatch.2021.1080p.bluray.x264.dts-hd.ma.5.1.dual.french.english-group",
                min: 0.75,
                max: -1.0,
            },
        ]);
    }

    // ── Sequel number edge: missing from release ──────────────────────

    #[test]
    fn test_sequel_missing_from_release() {
        // Want "Toy Story 5" but release is just "Toy Story" (no sequel number)
        // This should score lower than a proper match but the fuzzy matcher
        // alone can't definitively reject it — structural_reject handles that.
        let score = fuzzy_score("toy story 5 2026", "toy.story.2026.1080p.bluray.x264-group");
        assert!(
            score < 0.85,
            "Score should be reduced when sequel number is missing: got {}",
            score
        );
    }

    // ── Season identifier title word guard ────────────────────────────

    #[test]
    fn test_season_identifier_requires_title_word_match() {
        run_score_cases(&[
            // "Catch! Teenieping S06" must NOT match "To Catch a Smuggler S06"
            // — "catch" + "s06" match but "teenieping" doesn't, and that's
            // the distinguishing title word.
            ScoreCase {
                label: "teenieping vs smuggler (same season, different show)",
                pattern: "catch! teenieping s06",
                text: "to catch a smuggler s06 1080p hulu web-dl ddp5 1 h264-whitehat",
                min: -1.0,
                max: 0.75,
            },
            // Correct match must still work
            ScoreCase {
                label: "teenieping correct episode match",
                pattern: "catch! teenieping s06",
                text: "catch teenieping s06e20 1080p web-dl aac2.0 h.264-group",
                min: 0.75,
                max: -1.0,
            },
            // Another false positive: common word overlap but different show
            ScoreCase {
                label: "the good doctor vs good trouble (same season)",
                pattern: "the good doctor s08",
                text: "good.trouble.s08.1080p.web-dl.ddp5.1.h.264-group",
                min: -1.0,
                max: 0.75,
            },
            // But the correct show matches fine
            ScoreCase {
                label: "the good doctor correct match",
                pattern: "the good doctor s08",
                text: "the.good.doctor.s08e01.1080p.web-dl.ddp5.1.h.264-group",
                min: 0.75,
                max: -1.0,
            },
        ]);
    }

    #[test]
    fn test_bracket_tags_stripped_from_release() {
        run_score_cases(&[ScoreCase {
            label: "bracket tags stripped from release",
            pattern: "show name s01e05",
            text: "[yameii] show name s01e05 [1080p] [english dub]",
            min: 0.85,
            max: -1.0,
        }]);
    }

    #[test]
    fn test_is_season_episode_token() {
        assert!(is_season_episode_token("s06"));
        assert!(is_season_episode_token("s01e03"));
        assert!(is_season_episode_token("S22"));
        assert!(is_season_episode_token("s1"));
        assert!(is_season_episode_token("1x07"));
        assert!(!is_season_episode_token("catch"));
        assert!(!is_season_episode_token("anatomy"));
        assert!(!is_season_episode_token("2026"));
        assert!(!is_season_episode_token("the"));
        assert!(!is_season_episode_token(""));
    }
}
