//! Character-trigram similarity, as pg_trgm counts it: each word padded with
//! two spaces in front and one behind.

use std::collections::HashSet;

fn trigrams(s: &str) -> HashSet<[char; 3]> {
    let lower = s.to_lowercase();
    lower
        .split(|c: char| !c.is_alphanumeric())
        .filter(|w| !w.is_empty())
        .flat_map(|w| {
            let padded: Vec<char> = "  ".chars().chain(w.chars()).chain([' ']).collect();
            padded
                .windows(3)
                .map(|t| [t[0], t[1], t[2]])
                .collect::<Vec<_>>()
        })
        .collect()
}

/// How much of `name` the title carries, with plain Jaccard similarity as a
/// small tie-break so a title that is little else ranks first.
pub fn similarity(name: &str, title: &str) -> f64 {
    let (n, t) = (trigrams(name), trigrams(title));
    if n.is_empty() || t.is_empty() {
        return 0.0;
    }
    let shared = n.intersection(&t).count() as f64;
    let contained = shared / n.len() as f64;
    let jaccard = shared / n.union(&t).count() as f64;
    0.8 * contained + 0.2 * jaccard
}

pub fn best_similarity(names: &[String], title: &str) -> f64 {
    names
        .iter()
        .map(|n| similarity(n, title))
        .fold(0.0, f64::max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_title_that_names_the_want_ranks_first() {
        let name = "MotoGP Japan";
        let race = similarity(name, "MotoGP.2026.Japan.Race.1080p.WEB.h264-VERUM");
        let other_round = similarity(name, "MotoGP.2026.Thailand.Race.1080p.WEB.h264-VERUM");
        let unrelated = similarity(name, "Some.Show.S01E02.1080p.WEB.h264-GRP");
        assert!(
            race > other_round && other_round > unrelated,
            "{race} {other_round} {unrelated}"
        );
        assert!((similarity("abc", "abc") - 1.0).abs() < 1e-9);
    }
}
