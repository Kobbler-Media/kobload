//! The matcher as it was before titles and targets were prepared once per
//! release and per pattern. New code has to score every pair identically.

use kob_normalize::canonical;

use super::{
    MatchTarget, TargetMatchResult, is_season_episode_token, jaro_winkler, jw_digit_sanity_check,
    levenshtein,
};

fn fuzzy_score(pattern: &str, text: &str) -> f64 {
    title_similarity(pattern, text, &[])
}

/// Score a release title against multiple match targets.
/// Returns the best matching target (highest score above threshold).
pub fn score_against_targets(
    release_title: &str,
    targets: &[MatchTarget],
    default_min_score: f64,
) -> Option<TargetMatchResult> {
    let title_lower = release_title.to_lowercase();
    let sport_title = std::cell::OnceCell::new();

    targets
        .iter()
        .filter_map(|target| {
            let target_lower = target.target.to_lowercase();
            let mut score = fuzzy_score(&target_lower, &title_lower);
            // Sport wants and releases spell rounds, session numbers and places
            // differently ("R12 ... Race 1" vs "Round12 ... Race.One"); score the
            // canonical forms too, and keep the better of the two.
            if matches!(
                target.semantic_target,
                Some(nats_jobs::MatchedTarget::SportEvent)
            ) {
                let title =
                    sport_title.get_or_init(|| crate::parsing::canonical_sport_text(release_title));
                if let Some(canon_score) = sport_canonical_score(&target.target, title) {
                    score = score.max(canon_score);
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

/// Fuzzy score of a sport target against an already-canonical release title,
/// or `None` when the release is from another league.
///
/// Nothing downstream checks the league: once the round, place and session line
/// up, only the league token keeps a Moto2 want off the MotoGP release of the
/// same session, and an SBK want off World Supersport ("WSSP"). A token or two
/// short of a perfect score would still clear the floor, so the league has to
/// be present outright. It is everything before the season year in the target.
fn sport_canonical_score(target: &str, title_canon: &str) -> Option<f64> {
    let target_canon = crate::parsing::canonical_sport_text(target);
    let league: Vec<&str> = target_canon
        .split_whitespace()
        .take_while(|t| !(t.len() == 4 && t.bytes().all(|b| b.is_ascii_digit())))
        .collect();
    let in_title = |t: &&str| title_canon.split_whitespace().any(|tt| tt == *t);
    if league.is_empty() || !league.iter().all(in_title) {
        return None;
    }
    Some(fuzzy_score(&target_canon, title_canon))
}

/// Calculate a fuzzy match score between a pattern and text, dropping any
/// pattern tokens that match `stopwords` so articles like "the"/"in" don't
/// pad the match denominator. Uses a combination of techniques:
/// 1. Token overlap (words in common)
/// 2. Substring matching for each token
/// 3. Jaro-Winkler similarity for short patterns
pub(crate) fn title_similarity(pattern: &str, text: &str, stopwords: &[&str]) -> f64 {
    // Strip release group from text to prevent group names like
    // "samhydetorrentgalaxy" from polluting token matching.
    let text_stripped = crate::parsing::common::RELEASE_GROUP_RE.replace(text, "");
    // Canonicalize so dot-separated release titles ("FormulaE.2026.Round01.Brazil")
    // and underscore/hyphen-separated variants all tokenize into proper words.
    // This also handles diacritics, smart quotes, and ampersand expansion.
    let pattern_norm = canonical(pattern);
    let text_norm = canonical(&text_stripped);

    // Tokenize — keep single-digit numbers (sequel numbers like "5") but
    // filter other single-char tokens. Tokens are already free of punctuation
    // after canonicalization.
    let pattern_tokens: Vec<&str> = pattern_norm
        .split_whitespace()
        .filter(|t| t.len() >= 2 || t.chars().all(|c| c.is_ascii_digit()))
        .filter(|t| !stopwords.contains(t))
        .collect();
    let text_tokens: Vec<&str> = text_norm
        .split_whitespace()
        .filter(|t| t.len() >= 2 || t.chars().all(|c| c.is_ascii_digit()))
        .filter(|t| !t.starts_with('[') && !t.ends_with(']'))
        .collect();

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
    let full_pattern_match = if text_norm.contains(&pattern_norm) {
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

mod equivalence {
    use super::super::{
        MatchTarget, score_against_targets as new_score, title_similarity as new_sim,
    };
    use super::{score_against_targets as old_score, title_similarity as old_sim};
    use nats_jobs::MatchedTarget;
    use proptest::prelude::*;

    fn target(t: &str, semantic: Option<MatchedTarget>) -> MatchTarget {
        MatchTarget {
            id: "t".to_string(),
            target: t.to_string(),
            semantic_target: semantic,
            min_score: None,
        }
    }

    fn assert_same(title: &str, targets: &[MatchTarget]) {
        let old = old_score(title, targets, 0.0);
        let new = new_score(title, targets, 0.0);
        assert_eq!(
            old.as_ref().map(|r| (r.score.to_bits(), &r.matched_target)),
            new.as_ref().map(|r| (r.score.to_bits(), &r.matched_target)),
            "{title:?} vs {:?}",
            targets.iter().map(|t| &t.target).collect::<Vec<_>>()
        );
    }

    fn read_lines(path: &std::path::Path) -> Vec<String> {
        use std::io::BufRead;
        let file = std::fs::File::open(path).expect("open title corpus");
        std::io::BufReader::new(flate2::read::GzDecoder::new(file))
            .lines()
            .map(|l| l.expect("read title corpus"))
            .collect()
    }

    /// Every captured target against the matched titles, and a sample of
    /// targets against the DHT titles. Needs the local corpus:
    /// `KOBLOAD_TITLE_CORPUS=$PWD/data/scoring-corpus` and
    /// `KOBLOAD_SCORING_CORPUS=$PWD/data/scoring-capture`, in release mode.
    #[test]
    fn corpus_scores_match_the_reference() {
        let (Some(titles), Some(capture)) = (
            std::env::var_os("KOBLOAD_TITLE_CORPUS").map(std::path::PathBuf::from),
            std::env::var_os("KOBLOAD_SCORING_CORPUS").map(std::path::PathBuf::from),
        ) else {
            eprintln!("KOBLOAD_TITLE_CORPUS/KOBLOAD_SCORING_CORPUS not set; skipping");
            return;
        };
        let mut targets = std::collections::BTreeMap::new();
        for entry in std::fs::read_dir(capture.join("patterns")).expect("read patterns") {
            let file = std::fs::File::open(entry.expect("pattern entry").path()).expect("open");
            let patterns: Vec<serde_json::Value> =
                serde_json::from_reader(flate2::read::GzDecoder::new(file)).expect("parse");
            for p in patterns {
                for t in p["match_config"]["match_targets"]
                    .as_array()
                    .into_iter()
                    .flatten()
                {
                    let t: MatchTarget = serde_json::from_value(t.clone()).expect("target");
                    targets.insert((t.target.clone(), format!("{:?}", t.semantic_target)), t);
                }
            }
        }
        let targets: Vec<MatchTarget> = targets.into_values().collect();
        assert!(!targets.is_empty(), "no match targets in the capture");

        let matched = read_lines(&titles.join("titles-matched.txt.gz"));
        for title in &matched {
            for t in &targets {
                assert_same(title, std::slice::from_ref(t));
            }
            assert_same(title, &targets);
        }
        let sample: Vec<MatchTarget> = targets.iter().step_by(25).cloned().collect();
        for title in read_lines(&titles.join("titles-dht-sample.txt.gz")) {
            assert_same(&title, &sample);
        }
        eprintln!(
            "{} targets x {} matched titles, {} sampled targets x DHT titles",
            targets.len(),
            matched.len(),
            sample.len()
        );
    }

    fn token() -> impl Strategy<Value = String> {
        prop_oneof![
            "[A-Za-z]{1,10}",
            "[0-9]{1,4}",
            "[Ss][0-9]{2}[Ee][0-9]{2}",
            "(R|Round|Rd)[0-9]{1,2}",
            "(1080p|720p|2160p|WEB-DL|x264|H\\.264|DDP5\\.1)",
            "(Race|One|Two|FP1|Sprint|Superpole|GP|Grand|Prix)",
            "(MotoGP|Moto2|WSBK|SBK|WorldSBK|Formula1|F1|Czechia|Czech|Republic|Spanish|Great|Britain)",
            "[\\[(][A-Za-z0-9]{1,6}[\\])]",
            "(Café|Pokémon|Ñandú|Æon|Straße|日本|C\\+\\+|Rock&Roll|It's|Spider-Man)",
            "[+&'’!?]{1,2}",
        ]
    }

    fn joined(max: usize) -> impl Strategy<Value = String> {
        (
            prop::collection::vec(token(), 1..max),
            prop::collection::vec(prop_oneof![Just("."), Just(" "), Just("_"), Just("-")], max),
            "(-[A-Za-z0-9]{2,8}(\\.mkv)?)?",
        )
            .prop_map(|(tokens, seps, group)| {
                let mut s = String::new();
                for (i, t) in tokens.iter().enumerate() {
                    if i > 0 {
                        s.push_str(seps[i]);
                    }
                    s.push_str(t);
                }
                s + &group
            })
    }

    fn semantic() -> impl Strategy<Value = Option<MatchedTarget>> {
        prop_oneof![Just(None), Just(Some(MatchedTarget::SportEvent))]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(20_000))]

        #[test]
        fn random_titles_score_like_the_reference(
            title in joined(14),
            targets in prop::collection::vec((joined(7), semantic()), 1..5),
        ) {
            let targets: Vec<MatchTarget> =
                targets.iter().map(|(t, s)| target(t, s.clone())).collect();
            assert_same(&title, &targets);
        }

        #[test]
        fn title_similarity_with_stopwords_matches_the_reference(
            pattern in joined(7),
            text in joined(14),
        ) {
            let stopwords = ["the", "in", "a", "of"];
            prop_assert_eq!(
                old_sim(&pattern, &text, &stopwords).to_bits(),
                new_sim(&pattern, &text, &stopwords).to_bits()
            );
        }
    }
}
