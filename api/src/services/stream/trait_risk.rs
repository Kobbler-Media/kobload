use std::collections::HashMap;

use sqlx::PgPool;
use tracing::warn;
use uuid::Uuid;

use crate::repositories::stream_outcomes as repo;

pub const HALF_LIFE_SECS: f64 = 14.0 * 86_400.0;
/// Rows with no new evidence for this long are not read, and the cleanup job deletes them.
pub const TRAIT_TTL_DAYS: i32 = 60;
/// Decayed observations a trait needs before it moves anything: three, less the decay
/// they pick up in the time between being written and being read.
const MIN_EVIDENCE: f64 = 2.95;
/// Every observation also counts toward this trait, so a trait is judged against the
/// provider's overall refusal rate: a token on most releases ("1080p") tells nothing.
pub const BASELINE: &str = "all";

const MEDIUM_LIFT: f64 = 0.5;
const HIGH_LIFT: f64 = 1.5;

struct Token {
    text: String,
    sep_before: String,
}

fn tokens(title: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let mut sep = String::new();
    let mut word = String::new();
    for c in title.to_lowercase().chars() {
        if c.is_alphanumeric() {
            word.push(c);
        } else {
            if !word.is_empty() {
                out.push(Token {
                    text: std::mem::take(&mut word),
                    sep_before: std::mem::take(&mut sep),
                });
            }
            sep.push(c);
        }
    }
    if !word.is_empty() {
        out.push(Token {
            text: word,
            sep_before: sep,
        });
    }
    out
}

fn is_year(t: &str) -> bool {
    t.len() == 4
        && (t.starts_with("19") || t.starts_with("20"))
        && t.chars().all(|c| c.is_ascii_digit())
}

fn is_episode_marker(t: &str) -> bool {
    let rest = t.strip_prefix('s').or_else(|| t.strip_prefix('e'));
    rest.is_some_and(|r| {
        !r.is_empty()
            && r.split('e')
                .all(|p| !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()))
    })
}

fn is_resolution(t: &str) -> bool {
    t.strip_suffix('p')
        .is_some_and(|n| (3..=4).contains(&n.len()) && n.chars().all(|c| c.is_ascii_digit()))
}

/// What follows the title carries the release's own markings; the title's words would only
/// tie every release of one show together.
fn is_title_end(t: &str) -> bool {
    is_year(t) || is_episode_marker(t) || is_resolution(t) || t == "season"
}

fn is_noise(t: &str) -> bool {
    is_year(t) || is_episode_marker(t) || t.chars().all(|c| c.is_ascii_digit())
}

/// Tokens, adjacent pairs with the separator between them kept literally ("web-dl" and
/// "web.dl" are different traits, "webdl" a third), and the release group.
pub fn release_traits(title: &str) -> Vec<String> {
    let all = tokens(title);
    let mut traits = Vec::new();
    if let Some(start) = all.iter().position(|t| is_title_end(&t.text)) {
        let region = &all[start..];
        for (i, t) in region.iter().enumerate() {
            if is_noise(&t.text) {
                continue;
            }
            traits.push(format!("tok:{}", t.text));
            if let Some(next) = region.get(i + 1)
                && !is_noise(&next.text)
                && next.sep_before.chars().count() == 1
            {
                traits.push(format!("pair:{}{}{}", t.text, next.sep_before, next.text));
            }
        }
    }
    if let Some(group) = crate::quality::parse_quality(title).release_group {
        traits.push(format!("group:{}", group.to_lowercase()));
    }
    traits.sort();
    traits.dedup();
    traits
}

fn logit(p: f64) -> f64 {
    (p / (1.0 - p)).ln()
}

fn smoothed(failures: f64, successes: f64) -> f64 {
    (failures + 1.0) / (failures + successes + 2.0)
}

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Risk {
    /// Log-odds the release's traits add over the provider's baseline.
    pub lift: f64,
}

impl Risk {
    /// 0 = low, 1 = medium, 2 = high. Buckets rather than the raw value, so learned risk
    /// separates releases without reshuffling the quality order of near-equal ones.
    pub fn bucket(&self) -> u8 {
        if self.lift >= HIGH_LIFT {
            2
        } else if self.lift >= MEDIUM_LIFT {
            1
        } else {
            0
        }
    }
}

/// Learned per provider from its own refusals and acceptances. Nothing is assumed about any
/// provider or tag: with no evidence every release is neutral.
pub struct TraitRisk {
    lifts: HashMap<String, f64>,
}

impl TraitRisk {
    pub async fn load(pool: &PgPool, provider_id: Uuid, traits: &[String]) -> Self {
        let mut wanted = traits.to_vec();
        wanted.push(BASELINE.to_string());
        let rows = repo::traits_for(pool, provider_id, &wanted, HALF_LIFE_SECS, TRAIT_TTL_DAYS)
            .await
            .unwrap_or_else(|e| {
                warn!(error = %e, "trait outcome lookup failed; ranking neutrally");
                Vec::new()
            });
        let enough = |r: &repo::TraitRow| r.failure_weight + r.success_weight >= MIN_EVIDENCE;
        let baseline = rows
            .iter()
            .find(|r| r.trait_name == BASELINE && enough(r))
            .map_or(0.0, |r| logit(smoothed(r.failure_weight, r.success_weight)));
        let lifts = rows
            .iter()
            .filter(|r| r.trait_name != BASELINE && enough(r))
            .map(|r| {
                let lift = logit(smoothed(r.failure_weight, r.success_weight)) - baseline;
                (r.trait_name.clone(), lift)
            })
            .collect();
        Self { lifts }
    }

    pub fn risk(&self, traits: &[String]) -> Risk {
        let known: Vec<f64> = traits
            .iter()
            .filter_map(|t| self.lifts.get(t))
            .copied()
            .collect();
        if known.is_empty() {
            return Risk::default();
        }
        Risk {
            lift: known.iter().sum(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn traits_keep_literal_spellings_and_skip_the_title() {
        let cases: &[(&str, &[&str])] = &[
            (
                "Criminal.Minds.S01E01.1080p.WEB-DL.x264-SPARKS",
                &[
                    "group:sparks",
                    "pair:1080p.web",
                    "pair:dl.x264",
                    "pair:web-dl",
                    "pair:x264-sparks",
                    "tok:1080p",
                    "tok:dl",
                    "tok:sparks",
                    "tok:web",
                    "tok:x264",
                ],
            ),
            (
                "Movie 2024 WEBDL-GRP",
                &["group:grp", "pair:webdl-grp", "tok:grp", "tok:webdl"],
            ),
            ("No markers at all", &[]),
        ];
        for (title, want) in cases {
            assert_eq!(release_traits(title), *want, "{title}");
        }
    }

    #[test]
    fn a_trait_only_counts_above_the_baseline() {
        let risk = TraitRisk {
            lifts: HashMap::from([
                ("tok:bad".to_string(), logit(0.9)),
                ("tok:common".to_string(), 0.0),
            ]),
        };
        assert_eq!(risk.risk(&["tok:unseen".to_string()]), Risk::default());
        assert_eq!(risk.risk(&["tok:common".to_string()]).bucket(), 0);
        let bad = risk.risk(&["tok:bad".to_string(), "tok:common".to_string()]);
        assert_eq!(bad.bucket(), 2);
    }
}
