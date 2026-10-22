//! TRaSH blocklist matching as it was before plain patterns moved to `regex`
//! sets: every pattern a `fancy_regex`. The sets must report the same
//! category for every title.

use std::collections::{BTreeMap, HashMap, HashSet};

use fancy_regex::Regex as FancyRegex;
use tracing::warn;

use super::{compile_fancy, extract_simple_group_names};
use crate::repositories::trash_filters::TrashFilterRow;

pub(super) struct ReferenceSet {
    blocklist: Vec<CompiledCf>,
}

impl ReferenceSet {
    pub(super) fn check_blocklist(&self, title: &str, release_group: Option<&str>) -> Option<&str> {
        self.blocklist
            .iter()
            .find(|cf| cf.matches(title, release_group))
            .map(|cf| cf.category.as_str())
    }
}

struct CompiledCf {
    category: String,
    or_groups: HashSet<String>,
    or_title_patterns: Vec<FancyRegex>,
    and_specs: Vec<CompiledSpec>,
}

struct CompiledSpec {
    pattern: FancyRegex,
    is_group_spec: bool,
    negate: bool,
}

impl CompiledCf {
    fn matches(&self, title: &str, release_group: Option<&str>) -> bool {
        // 1. Evaluate required (AND) specs — all must match.
        let has_required = !self.and_specs.is_empty();
        if has_required {
            for spec in &self.and_specs {
                let input = if spec.is_group_spec {
                    release_group.unwrap_or("")
                } else {
                    title
                };
                let matched = spec.pattern.is_match(input).unwrap_or(false);
                let result = if spec.negate { !matched } else { matched };
                if !result {
                    return false;
                }
            }
        }

        // 2. Evaluate optional (OR) specs — any must match.
        let has_optional = !self.or_groups.is_empty() || !self.or_title_patterns.is_empty();
        if has_optional {
            let group_lower = release_group.map(|g| g.to_ascii_lowercase());
            if let Some(ref g) = group_lower
                && self.or_groups.contains(g.as_str())
            {
                return true;
            }
            for pat in &self.or_title_patterns {
                if pat.is_match(title).unwrap_or(false) {
                    return true;
                }
            }
            // Optional specs exist but none matched.
            return false;
        }

        // 3. No optional specs — result depends solely on required.
        has_required
    }
}

pub(super) fn compile(rows: &[TrashFilterRow]) -> ReferenceSet {
    // Partition rows by category. Ordered, because `check_blocklist` reports the
    // first matching category; with a HashMap that varied between restarts.
    let mut by_category: BTreeMap<&str, Vec<&TrashFilterRow>> = BTreeMap::new();
    for row in rows {
        by_category.entry(&row.category).or_default().push(row);
    }

    let mut blocklist = Vec::new();
    let mut tier_bonuses: HashMap<String, f64> = HashMap::new();

    for (category, cat_rows) in &by_category {
        let is_tier = cat_rows.iter().any(|r| r.tier_bonus.is_some());

        if is_tier {
            // Tier category — extract group names and map to bonus values.
            for row in cat_rows {
                if row.filter_type != "release_group" {
                    continue;
                }
                let bonus = row.tier_bonus.unwrap_or(0.0);
                if let Some(names) = extract_simple_group_names(&row.pattern) {
                    for name in names {
                        let existing = tier_bonuses.get(&name).copied().unwrap_or(0.0);
                        if bonus > existing {
                            tier_bonuses.insert(name, bonus);
                        }
                    }
                } else if let Ok(re) = FancyRegex::new(&row.pattern) {
                    // Complex pattern — try to extract the group name by testing
                    // common known groups isn't feasible. Log and skip.
                    // For tier sources, patterns are almost always simple `^(Name)$`.
                    warn!(
                        pattern = row.pattern,
                        "Complex tier group pattern, cannot extract names — skipping"
                    );
                    drop(re);
                }
            }
        } else {
            // Blocklist category — build a CompiledCf.
            let mut or_groups = HashSet::new();
            let mut or_title_patterns = Vec::new();
            let mut and_specs = Vec::new();

            for row in cat_rows {
                let is_group = row.filter_type == "release_group";

                if row.required {
                    // Required (AND) spec.
                    match compile_fancy(&row.pattern) {
                        Ok(re) => {
                            and_specs.push(CompiledSpec {
                                pattern: re,
                                is_group_spec: is_group,
                                negate: row.negate,
                            });
                        }
                        Err(e) => {
                            warn!(
                                pattern = row.pattern,
                                error = %e,
                                "Failed to compile TRaSH filter regex — skipping"
                            );
                        }
                    }
                } else if is_group && !row.negate {
                    // Optional group spec — try O(1) HashSet path first.
                    if let Some(names) = extract_simple_group_names(&row.pattern) {
                        for name in names {
                            or_groups.insert(name);
                        }
                    } else {
                        // Fallback: compile as regex, but treat as an AND
                        // spec with negate=false so it runs against the group.
                        match compile_fancy(&row.pattern) {
                            Ok(re) => {
                                and_specs.push(CompiledSpec {
                                    pattern: re,
                                    is_group_spec: true,
                                    negate: false,
                                });
                            }
                            Err(e) => {
                                warn!(
                                    pattern = row.pattern,
                                    error = %e,
                                    "Failed to compile TRaSH filter regex — skipping"
                                );
                            }
                        }
                    }
                } else if !is_group && !row.negate {
                    // Optional title spec.
                    match compile_fancy(&row.pattern) {
                        Ok(re) => or_title_patterns.push(re),
                        Err(e) => {
                            warn!(
                                pattern = row.pattern,
                                error = %e,
                                "Failed to compile TRaSH filter regex — skipping"
                            );
                        }
                    }
                } else {
                    // Optional negated spec — treat as required (AND) to
                    // preserve the negate semantics.
                    match compile_fancy(&row.pattern) {
                        Ok(re) => {
                            and_specs.push(CompiledSpec {
                                pattern: re,
                                is_group_spec: is_group,
                                negate: row.negate,
                            });
                        }
                        Err(e) => {
                            warn!(
                                pattern = row.pattern,
                                error = %e,
                                "Failed to compile TRaSH filter regex — skipping"
                            );
                        }
                    }
                }
            }

            blocklist.push(CompiledCf {
                category: (*category).to_owned(),
                or_groups,
                or_title_patterns,
                and_specs,
            });
        }
    }

    drop(tier_bonuses);
    ReferenceSet { blocklist }
}

mod equivalence {
    use proptest::prelude::*;

    use super::super::TrashFilterSet;
    use super::{ReferenceSet, compile};
    use crate::quality::parse_quality;
    use crate::repositories::trash_filters::TrashFilterRow;

    fn captured_rows() -> Option<Vec<TrashFilterRow>> {
        let dir = std::path::PathBuf::from(std::env::var_os("KOBLOAD_SCORING_CORPUS")?);
        let mut rows = Vec::new();
        for entry in std::fs::read_dir(dir.join("trash")).expect("read trash dir") {
            let file = std::fs::File::open(entry.expect("trash entry").path()).expect("open");
            let part: Vec<TrashFilterRow> =
                serde_json::from_reader(flate2::read::GzDecoder::new(file)).expect("parse");
            rows.extend(part);
        }
        Some(rows)
    }

    fn assert_same(old: &ReferenceSet, new: &TrashFilterSet, title: &str) {
        let group = parse_quality(title).release_group;
        for group in [group.as_deref(), None] {
            assert_eq!(
                old.check_blocklist(title, group),
                new.check_blocklist(title, group),
                "{title:?} group {group:?}"
            );
        }
    }

    /// Every release title in the capture, with its parsed group and with none.
    /// Needs `KOBLOAD_SCORING_CORPUS=$PWD/data/scoring-capture`, in release mode.
    #[test]
    fn captured_titles_hit_the_same_category() {
        let Some(rows) = captured_rows() else {
            eprintln!("KOBLOAD_SCORING_CORPUS not set; skipping");
            return;
        };
        let (old, new) = (compile(&rows), TrashFilterSet::from_rows(&rows));
        let dir = std::path::PathBuf::from(std::env::var_os("KOBLOAD_SCORING_CORPUS").unwrap());
        let titles: std::collections::BTreeSet<String> = crate::watch::capture::read_capture(&dir)
            .expect("read capture")
            .into_iter()
            .flat_map(|f| f.fetch.releases.into_iter().map(|r| r.title.clone()))
            .collect();
        let blocked = titles
            .iter()
            .inspect(|t| assert_same(&old, &new, t))
            .filter(|t| {
                new.check_blocklist(t, parse_quality(t).release_group.as_deref())
                    .is_some()
            })
            .count();
        eprintln!("{} titles, {blocked} blocklisted", titles.len());
    }

    fn token() -> impl Strategy<Value = String> {
        prop_oneof![
            "[A-Za-z]{1,8}",
            "[0-9]{1,4}",
            "(1080p|720p|2160p|480p|BluRay|BDRip|BR-?DISK|COMPLETE|AVC|HEVC|x26[45]|[XH]\\.?26[45]|REMUX|WEB-?DL|HDTV|DVD9|ISO|M2TS|BDMV)",
            "(YIFY|YTS|RARBG|EVO|aXXo|Pahe|MeGusta|NAHOM|TGx|EZTV|SubsPlease|Erai-raws|Judas|Tigole|QxR|PSA|Sample|Extras|Featurette|Dubbed|LiNE|MIC|DUAL)",
            "[\\[(][A-Za-z0-9 .-]{1,10}[\\])]",
            "(Café|Pokémon|日本|C\\+\\+|Rock&Roll|It's)",
        ]
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(5_000))]

        #[test]
        fn random_titles_hit_the_same_category(
            tokens in prop::collection::vec(token(), 1..12),
            sep in prop_oneof![Just("."), Just(" "), Just("_"), Just("-")],
            group in "(-[A-Za-z0-9]{2,8})?",
        ) {
            let Some(rows) = captured_rows() else {
                return Ok(());
            };
            thread_local! {
                static SETS: std::cell::OnceCell<(ReferenceSet, TrashFilterSet)> =
                    const { std::cell::OnceCell::new() };
            }
            let title = tokens.join(sep) + &group;
            SETS.with(|sets| {
                let (old, new) = sets.get_or_init(|| (compile(&rows), TrashFilterSet::from_rows(&rows)));
                assert_same(old, new, &title);
            });
        }
    }
}
