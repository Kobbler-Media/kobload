use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use fancy_regex::Regex as FancyRegex;
use reqwest::Client;
use serde::Deserialize;
use sqlx::PgPool;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use crate::repositories::trash_filters as trash_repo;
use crate::repositories::trash_filters::TrashFilterRow;

const REFRESH_TTL_SECS: i64 = 24 * 60 * 60; // 24 hours

const TRASH_BASE_URL: &str =
    "https://raw.githubusercontent.com/TRaSH-Guides/Guides/master/docs/json/";

/// Blocklist CF sources: (category, &[relative_paths]).
const TRASH_CF_SOURCES: &[(&str, &[&str])] = &[
    ("lq", &["radarr/cf/lq.json", "sonarr/cf/lq.json"]),
    (
        "lq_title",
        &[
            "radarr/cf/lq-release-title.json",
            "sonarr/cf/lq-release-title.json",
        ],
    ),
    (
        "upscaled",
        &["radarr/cf/upscaled.json", "sonarr/cf/upscaled.json"],
    ),
    (
        "br_disk",
        &["radarr/cf/br-disk.json", "sonarr/cf/br-disk.json"],
    ),
    (
        "bad_dual",
        &[
            "radarr/cf/bad-dual-groups.json",
            "sonarr/cf/bad-dual-groups.json",
        ],
    ),
    ("line_mic", &["radarr/cf/line-mic-dubbed.json"]),
    (
        "obfuscated",
        &["radarr/cf/obfuscated.json", "sonarr/cf/obfuscated.json"],
    ),
    (
        "retags",
        &["radarr/cf/retags.json", "sonarr/cf/retags.json"],
    ),
    (
        "x265_hd",
        &["radarr/cf/x265-hd.json", "sonarr/cf/x265-hd.json"],
    ),
    (
        "extras",
        &["radarr/cf/extras.json", "sonarr/cf/extras.json"],
    ),
    (
        "no_group",
        &["radarr/cf/no-rlsgroup.json", "sonarr/cf/no-rlsgroup.json"],
    ),
    ("anime_lq", &["sonarr/cf/anime-lq-groups.json"]),
];

/// Tier CF sources: (category, bonus_value, &[relative_paths]).
const TRASH_TIER_SOURCES: &[(&str, f64, &[&str])] = &[
    (
        "web_tier",
        0.10,
        &["radarr/cf/web-tier-01.json", "sonarr/cf/web-tier-01.json"],
    ),
    (
        "web_tier",
        0.06,
        &["radarr/cf/web-tier-02.json", "sonarr/cf/web-tier-02.json"],
    ),
    (
        "web_tier",
        0.03,
        &["radarr/cf/web-tier-03.json", "sonarr/cf/web-tier-03.json"],
    ),
    (
        "remux_tier",
        0.10,
        &[
            "radarr/cf/remux-tier-01.json",
            "sonarr/cf/remux-tier-01.json",
        ],
    ),
    (
        "remux_tier",
        0.06,
        &[
            "radarr/cf/remux-tier-02.json",
            "sonarr/cf/remux-tier-02.json",
        ],
    ),
    ("remux_tier", 0.03, &["radarr/cf/remux-tier-03.json"]),
    (
        "hd_bluray_tier",
        0.10,
        &[
            "radarr/cf/hd-bluray-tier-01.json",
            "sonarr/cf/hd-bluray-tier-01.json",
        ],
    ),
    (
        "hd_bluray_tier",
        0.06,
        &[
            "radarr/cf/hd-bluray-tier-02.json",
            "sonarr/cf/hd-bluray-tier-02.json",
        ],
    ),
    (
        "hd_bluray_tier",
        0.03,
        &["radarr/cf/hd-bluray-tier-03.json"],
    ),
    (
        "uhd_bluray_tier",
        0.10,
        &["radarr/cf/uhd-bluray-tier-01.json"],
    ),
    (
        "uhd_bluray_tier",
        0.06,
        &["radarr/cf/uhd-bluray-tier-02.json"],
    ),
    (
        "uhd_bluray_tier",
        0.03,
        &["radarr/cf/uhd-bluray-tier-03.json"],
    ),
];

// ---------------------------------------------------------------------------
// JSON deserialization types for TRaSH CF files
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct TrashCfFile {
    #[serde(default)]
    specifications: Vec<TrashSpec>,
}

#[derive(Deserialize)]
struct TrashSpec {
    name: String,
    implementation: String,
    #[serde(default)]
    negate: bool,
    #[serde(default)]
    required: bool,
    fields: TrashSpecFields,
}

#[derive(Deserialize)]
struct TrashSpecFields {
    value: serde_json::Value,
}

// ---------------------------------------------------------------------------
// Compiled in-memory filter set
// ---------------------------------------------------------------------------

pub struct TrashFilterSet {
    blocklist: Vec<CompiledCf>,
    tier_bonuses: HashMap<String, f64>,
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

impl TrashFilterSet {
    /// Check blocklist. Returns `Some(category)` on first match.
    pub fn check_blocklist(&self, title: &str, release_group: Option<&str>) -> Option<&str> {
        for cf in &self.blocklist {
            if cf.matches(title, release_group) {
                return Some(&cf.category);
            }
        }
        None
    }

    /// Get tier bonus for release group. Returns `0.0` if unknown.
    pub fn tier_bonus(&self, release_group: Option<&str>) -> f64 {
        release_group
            .and_then(|g| self.tier_bonuses.get(&g.to_ascii_lowercase()))
            .copied()
            .unwrap_or(0.0)
    }

    /// Build a filter set with only tier bonuses, for cross-module tests.
    #[cfg(test)]
    pub fn from_tier_bonuses(bonuses: &[(&str, f64)]) -> Self {
        Self {
            blocklist: Vec::new(),
            tier_bonuses: bonuses
                .iter()
                .map(|(g, b)| (g.to_ascii_lowercase(), *b))
                .collect(),
        }
    }

    /// Compile a filter set from stored rows, as the cache does on load.
    pub fn from_rows(rows: &[TrashFilterRow]) -> Self {
        compile(rows)
    }

    /// Returns (blocklist_rule_count, tier_bonus_count).
    pub fn stats(&self) -> (usize, usize) {
        (self.blocklist.len(), self.tier_bonuses.len())
    }
}

// ---------------------------------------------------------------------------
// Group name extraction optimisation
// ---------------------------------------------------------------------------

/// Try to extract simple literal group names from a pattern like `^(YIFY|YTS)$`.
///
/// Returns `Some(names)` only when the inner content is purely alphanumeric
/// characters and `|` separators — no regex metacharacters.
fn extract_simple_group_names(pattern: &str) -> Option<Vec<String>> {
    let inner = pattern.strip_prefix("^(")?.strip_suffix(")$")?;
    // Reject if any regex metacharacter is present (beyond `|`).
    for ch in inner.chars() {
        if !ch.is_ascii_alphanumeric() && ch != '|' && ch != '-' && ch != '_' {
            return None;
        }
    }
    Some(inner.split('|').map(|s| s.to_ascii_lowercase()).collect())
}

/// Attempt to rewrite variable-length look-behind assertions into equivalent
/// inline patterns so `fancy_regex` can compile them.
///
/// TRaSH Guide patterns are authored for .NET / PCRE which support variable-
/// length look-behind. `fancy_regex` only supports fixed-width look-behind.
///
/// Strategy:
///   - `(?<=X)` (positive) → `(?:X)` — include the prefix as part of the match.
///   - `(?<!X)` (negative) → dropped — makes the pattern slightly more
///     aggressive, which is the safe direction for a blocklist.
///
/// Returns `None` if no look-behind is found.
fn rewrite_lookbehind(pattern: &str) -> Option<String> {
    if !pattern.contains("(?<=") && !pattern.contains("(?<!") {
        return None;
    }

    let mut result = String::with_capacity(pattern.len());
    let bytes = pattern.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        if i + 4 <= len && &bytes[i..i + 4] == b"(?<=" {
            // Positive look-behind → non-capturing group.
            result.push_str("(?:");
            i += 4;
            // Copy until the matching closing paren, accounting for nesting.
            let mut depth = 1u32;
            while i < len && depth > 0 {
                match bytes[i] {
                    b'(' => {
                        depth += 1;
                        result.push('(');
                    }
                    b')' => {
                        depth -= 1;
                        result.push(')');
                    }
                    b'\\' if i + 1 < len => {
                        result.push('\\');
                        result.push(bytes[i + 1] as char);
                        i += 1;
                    }
                    c => result.push(c as char),
                }
                i += 1;
            }
        } else if i + 4 <= len && &bytes[i..i + 4] == b"(?<!" {
            // Negative look-behind → drop entirely (including contents).
            i += 4;
            let mut depth = 1u32;
            while i < len && depth > 0 {
                match bytes[i] {
                    b'(' => depth += 1,
                    b')' => depth -= 1,
                    b'\\' if i + 1 < len => {
                        i += 1;
                    }
                    _ => {}
                }
                i += 1;
            }
        } else {
            result.push(bytes[i] as char);
            i += 1;
        }
    }

    Some(result)
}

/// Compile a regex pattern, falling back to look-behind rewriting if needed.
fn compile_fancy(pattern: &str) -> Result<FancyRegex, Box<fancy_regex::Error>> {
    match FancyRegex::new(pattern) {
        Ok(re) => Ok(re),
        Err(e) => {
            let msg = e.to_string();
            if (msg.contains("Look-behind") || msg.contains("look-behind"))
                && let Some(rewritten) = rewrite_lookbehind(pattern)
            {
                debug!(
                    original = pattern,
                    rewritten = rewritten.as_str(),
                    "Rewrote variable-length look-behind pattern"
                );
                return FancyRegex::new(&rewritten).map_err(Box::new);
            }
            Err(Box::new(e))
        }
    }
}

// ---------------------------------------------------------------------------
// Cache wrapper
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct TrashFilterCache {
    data: Arc<RwLock<Option<Arc<TrashFilterSet>>>>,
    refreshing: Arc<AtomicBool>,
}

impl Default for TrashFilterCache {
    fn default() -> Self {
        Self::new()
    }
}

impl TrashFilterCache {
    pub fn new() -> Self {
        Self {
            data: Arc::new(RwLock::new(None)),
            refreshing: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Load cache from DB on startup. If DB is empty or stale, fetch from API
    /// synchronously. If API fails and DB has data, use stale data. If both
    /// fail, log and continue with empty cache.
    pub async fn ensure_loaded(&self, client: &Client, pool: &PgPool) {
        let last_fetched = trash_repo::get_last_fetched(pool).await.ok().flatten();

        if let Some(fetched_at) = last_fetched {
            match trash_repo::get_all(pool).await {
                Ok(rows) if !rows.is_empty() => {
                    let filter_set = compile(&rows);
                    let bl = filter_set.blocklist.len();
                    let tb = filter_set.tier_bonuses.len();
                    {
                        let mut data = self.data.write().await;
                        *data = Some(Arc::new(filter_set));
                    }
                    info!(
                        blocklist_rules = bl,
                        tier_bonuses = tb,
                        "TRaSH filter cache loaded from DB"
                    );

                    let age = time::OffsetDateTime::now_utc() - fetched_at;
                    if age.whole_seconds() > REFRESH_TTL_SECS {
                        self.trigger_refresh(client.clone(), pool.clone());
                    }
                    return;
                }
                Ok(_) => {} // empty rows, fall through
                Err(e) => {
                    warn!(error = %e, "Failed to load TRaSH filters from DB");
                }
            }
        }

        // No DB data — fetch synchronously from API.
        if let Some(filter_set) = fetch_and_store(client, pool).await {
            let bl = filter_set.blocklist.len();
            let tb = filter_set.tier_bonuses.len();
            let mut data = self.data.write().await;
            *data = Some(Arc::new(filter_set));
            info!(
                blocklist_rules = bl,
                tier_bonuses = tb,
                "TRaSH filter cache loaded from API"
            );
        } else {
            warn!("TRaSH filter cache: API unavailable and no DB data, starting with empty cache");
        }
    }

    /// Try-read the cached filter set (non-blocking).
    pub fn get_filters(&self) -> Option<Arc<TrashFilterSet>> {
        match self.data.try_read() {
            Ok(guard) => guard.clone(),
            Err(_) => None,
        }
    }

    /// Force a synchronous refresh (used by sync job handler).
    pub async fn refresh(&self, client: &Client, pool: &PgPool) {
        if let Some(filter_set) = fetch_and_store(client, pool).await {
            let mut guard = self.data.write().await;
            *guard = Some(Arc::new(filter_set));
        }
    }

    fn trigger_refresh(&self, client: Client, pool: PgPool) {
        if self
            .refreshing
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return;
        }

        let data = Arc::clone(&self.data);
        let refreshing = Arc::clone(&self.refreshing);

        tokio::spawn(async move {
            if let Some(filter_set) = fetch_and_store(&client, &pool).await {
                let bl = filter_set.blocklist.len();
                let tb = filter_set.tier_bonuses.len();
                let mut guard = data.write().await;
                *guard = Some(Arc::new(filter_set));
                debug!(
                    blocklist_rules = bl,
                    tier_bonuses = tb,
                    "TRaSH filter cache refreshed in background"
                );
            } else {
                warn!("TRaSH filter cache background refresh failed, serving stale data");
            }
            refreshing.store(false, Ordering::SeqCst);
        });
    }
}

// ---------------------------------------------------------------------------
// Fetch, parse, store, compile
// ---------------------------------------------------------------------------

/// Fetch all TRaSH CF sources, store in DB, return compiled filter set.
async fn fetch_and_store(client: &Client, pool: &PgPool) -> Option<TrashFilterSet> {
    info!("Fetching TRaSH Guides filters");

    let mut db_rows: Vec<TrashFilterRow> = Vec::new();

    // --- Blocklist sources ---
    for &(category, paths) in TRASH_CF_SOURCES {
        let specs = fetch_cf_specs(client, paths).await;
        for spec in specs {
            if let Some(pattern) = spec.fields.value.as_str() {
                let filter_type = match spec.implementation.as_str() {
                    "ReleaseGroupSpecification" => "release_group",
                    "ReleaseTitleSpecification" => "release_title",
                    _ => continue,
                };
                db_rows.push(TrashFilterRow {
                    category: category.to_owned(),
                    spec_name: spec.name,
                    filter_type: filter_type.to_owned(),
                    pattern: pattern.to_owned(),
                    negate: spec.negate,
                    required: spec.required,
                    tier_bonus: None,
                });
            }
        }
    }

    // --- Tier sources ---
    for &(category, bonus, paths) in TRASH_TIER_SOURCES {
        let specs = fetch_cf_specs(client, paths).await;
        for spec in specs {
            if let Some(pattern) = spec.fields.value.as_str() {
                let filter_type = match spec.implementation.as_str() {
                    "ReleaseGroupSpecification" => "release_group",
                    "ReleaseTitleSpecification" => "release_title",
                    _ => continue,
                };
                db_rows.push(TrashFilterRow {
                    category: category.to_owned(),
                    spec_name: spec.name,
                    filter_type: filter_type.to_owned(),
                    pattern: pattern.to_owned(),
                    negate: spec.negate,
                    required: spec.required,
                    tier_bonus: Some(bonus),
                });
            }
        }
    }

    // Deduplicate within each category by (spec_name, pattern).
    dedup_rows(&mut db_rows);

    if db_rows.is_empty() {
        warn!("No TRaSH filter specs fetched — all sources may be unavailable");
        return None;
    }

    info!(specs = db_rows.len(), "Fetched TRaSH Guides filter specs");

    if let Err(e) = trash_repo::replace_all(pool, &db_rows).await {
        warn!(error = %e, "Failed to store TRaSH filters in DB");
    }
    if let Err(e) = trash_repo::set_last_fetched(pool).await {
        warn!(error = %e, "Failed to update TRaSH filter last_fetched");
    }

    Some(compile(&db_rows))
}

/// Fetch and parse CF specs from multiple URLs for a single category, deduped.
async fn fetch_cf_specs(client: &Client, paths: &[&str]) -> Vec<TrashSpec> {
    let futures: Vec<_> = paths
        .iter()
        .map(|path| {
            let url = format!("{TRASH_BASE_URL}{path}");
            let client = client.clone();
            async move { fetch_one_cf(&client, &url).await }
        })
        .collect();

    let results = futures::future::join_all(futures).await;

    let mut specs = Vec::new();
    for result in results {
        specs.extend(result);
    }
    specs
}

/// Fetch a single CF JSON file and return its specifications.
async fn fetch_one_cf(client: &Client, url: &str) -> Vec<TrashSpec> {
    let resp = match client.get(url).send().await {
        Ok(r) => r,
        Err(e) => {
            warn!(url, error = %e, "Failed to fetch TRaSH CF file");
            return Vec::new();
        }
    };

    if !resp.status().is_success() {
        warn!(url, status = %resp.status(), "TRaSH CF file request failed");
        return Vec::new();
    }

    match resp.json::<TrashCfFile>().await {
        Ok(cf) => cf.specifications,
        Err(e) => {
            warn!(url, error = %e, "Failed to parse TRaSH CF file");
            Vec::new()
        }
    }
}

/// Remove duplicate (category, spec_name, pattern) entries, keeping the first.
fn dedup_rows(rows: &mut Vec<TrashFilterRow>) {
    let mut seen = HashSet::new();
    rows.retain(|row| {
        seen.insert((
            row.category.clone(),
            row.spec_name.clone(),
            row.pattern.clone(),
        ))
    });
}

// ---------------------------------------------------------------------------
// Compilation from DB rows into in-memory filter set
// ---------------------------------------------------------------------------

fn compile(rows: &[TrashFilterRow]) -> TrashFilterSet {
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

    info!(
        blocklist_rules = blocklist.len(),
        tier_bonuses = tier_bonuses.len(),
        "Compiled TRaSH filter set"
    );

    TrashFilterSet {
        blocklist,
        tier_bonuses,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_simple_group_names_single() {
        let names = extract_simple_group_names("^(YIFY)$").unwrap();
        assert_eq!(names, vec!["yify"]);
    }

    #[test]
    fn test_extract_simple_group_names_multiple() {
        let names = extract_simple_group_names("^(YIFY|YTS|RARBG)$").unwrap();
        assert_eq!(names, vec!["yify", "yts", "rarbg"]);
    }

    #[test]
    fn test_extract_simple_group_names_with_dash() {
        let names = extract_simple_group_names("^(DON-group)$").unwrap();
        assert_eq!(names, vec!["don-group"]);
    }

    #[test]
    fn test_extract_simple_group_names_rejects_complex() {
        assert!(extract_simple_group_names("^(YTS(.MX)?)$").is_none());
        assert!(extract_simple_group_names("YIFY").is_none());
        assert!(extract_simple_group_names("^YIFY$").is_none());
        assert!(extract_simple_group_names("^(YIFY.*|YTS)$").is_none());
        assert!(extract_simple_group_names("^(name\\d+)$").is_none());
    }

    #[test]
    fn test_rewrite_lookbehind_positive() {
        let pattern = r"(?<=\bS\d+\b).*\b(Extras|Bonus|Extended[ ._-]Clip)\b";
        let rewritten = rewrite_lookbehind(pattern).unwrap();
        assert_eq!(
            rewritten,
            r"(?:\bS\d+\b).*\b(Extras|Bonus|Extended[ ._-]Clip)\b"
        );
        // Must compile successfully.
        FancyRegex::new(&rewritten).unwrap();
    }

    #[test]
    fn test_rewrite_lookbehind_negative_dropped() {
        let pattern = r"(?<=\b[12]\d{3}\b.*?)(?<!\b(web[ ._-]?(dl|rip)?).*?)\b(EVO)\b";
        let rewritten = rewrite_lookbehind(pattern).unwrap();
        assert_eq!(rewritten, r"(?:\b[12]\d{3}\b.*?)\b(EVO)\b");
        FancyRegex::new(&rewritten).unwrap();
    }

    #[test]
    fn test_rewrite_lookbehind_none_when_absent() {
        assert!(rewrite_lookbehind(r"^(YIFY|YTS)$").is_none());
    }

    #[test]
    fn test_compile_fancy_falls_back() {
        // This pattern fails with plain FancyRegex::new but succeeds via rewrite.
        let re = compile_fancy(r"(?<=\bS\d+\b).*(Scrambled)\b").unwrap();
        assert!(re.is_match("Show.S01.Scrambled").unwrap());
        assert!(!re.is_match("Show.Scrambled").unwrap()); // no season prefix
    }

    #[test]
    fn test_compile_fancy_ai_upscaled() {
        let re = compile_fancy(
            r"(?<=\b\d{3,4}p\b).*\b(AI[ ._-]?Enhanced?|UPS(UHD)?|Upscaled?([ ._-]?UHD)?|UpRez)\b",
        )
        .unwrap();
        assert!(re.is_match("Movie.2024.1080p.AI.Enhanced").unwrap());
        assert!(!re.is_match("Movie.2024.AI.Enhanced").unwrap()); // no resolution
    }

    #[test]
    fn test_parse_trash_cf_file() {
        let json = r#"{
            "specifications": [
                {
                    "name": "YIFY",
                    "implementation": "ReleaseGroupSpecification",
                    "negate": false,
                    "required": false,
                    "fields": { "value": "^(YIFY|YTS)$" }
                },
                {
                    "name": "Bad Title",
                    "implementation": "ReleaseTitleSpecification",
                    "negate": false,
                    "required": false,
                    "fields": { "value": "\\b(CAMRip|HDTS)\\b" }
                },
                {
                    "name": "Resolution Check",
                    "implementation": "ResolutionSpecification",
                    "negate": false,
                    "required": true,
                    "fields": { "value": 1080 }
                }
            ]
        }"#;

        let cf: TrashCfFile = serde_json::from_str(json).unwrap();
        assert_eq!(cf.specifications.len(), 3);
        assert_eq!(
            cf.specifications[0].implementation,
            "ReleaseGroupSpecification"
        );
        assert!(cf.specifications[0].fields.value.is_string());
        assert!(cf.specifications[2].fields.value.is_number());
    }

    #[test]
    fn test_compiled_cf_matches_or_group() {
        let cf = CompiledCf {
            category: "lq".to_owned(),
            or_groups: HashSet::from(["yify".to_owned(), "yts".to_owned()]),
            or_title_patterns: Vec::new(),
            and_specs: Vec::new(),
        };

        assert!(cf.matches("Some.Movie.2024.1080p", Some("YIFY")));
        assert!(cf.matches("Some.Movie.2024.1080p", Some("yts")));
        assert!(!cf.matches("Some.Movie.2024.1080p", Some("NTb")));
        assert!(!cf.matches("Some.Movie.2024.1080p", None));
    }

    #[test]
    fn test_compiled_cf_matches_or_title() {
        let cf = CompiledCf {
            category: "lq_title".to_owned(),
            or_groups: HashSet::new(),
            or_title_patterns: vec![FancyRegex::new(r"\b(CAMRip)\b").unwrap()],
            and_specs: Vec::new(),
        };

        assert!(cf.matches("Movie.2024.CAMRip.x264", None));
        assert!(!cf.matches("Movie.2024.BluRay.x264", None));
    }

    #[test]
    fn test_compiled_cf_matches_required_and() {
        let cf = CompiledCf {
            category: "x265_hd".to_owned(),
            or_groups: HashSet::new(),
            or_title_patterns: Vec::new(),
            and_specs: vec![
                CompiledSpec {
                    pattern: FancyRegex::new(r"(?i)\bx265\b").unwrap(),
                    is_group_spec: false,
                    negate: false,
                },
                CompiledSpec {
                    // Negate: must NOT be 2160p
                    pattern: FancyRegex::new(r"(?i)\b2160p\b").unwrap(),
                    is_group_spec: false,
                    negate: true,
                },
            ],
        };

        // x265 and NOT 2160p → match
        assert!(cf.matches("Movie.2024.1080p.x265", None));
        // x265 and 2160p → no match (negate fails)
        assert!(!cf.matches("Movie.2024.2160p.x265", None));
        // no x265 → no match
        assert!(!cf.matches("Movie.2024.1080p.x264", None));
    }

    #[test]
    fn test_compiled_cf_matches_required_plus_optional() {
        let cf = CompiledCf {
            category: "test".to_owned(),
            or_groups: HashSet::from(["badgrp".to_owned()]),
            or_title_patterns: Vec::new(),
            and_specs: vec![CompiledSpec {
                pattern: FancyRegex::new(r"(?i)\b720p\b").unwrap(),
                is_group_spec: false,
                negate: false,
            }],
        };

        // Required passes AND optional matches → match
        assert!(cf.matches("Movie.720p", Some("BadGrp")));
        // Required passes but no optional match → no match
        assert!(!cf.matches("Movie.720p", Some("GoodGrp")));
        // Required fails → no match regardless
        assert!(!cf.matches("Movie.1080p", Some("BadGrp")));
    }

    #[test]
    fn test_trash_filter_set_blocklist() {
        let fs = TrashFilterSet {
            blocklist: vec![CompiledCf {
                category: "lq".to_owned(),
                or_groups: HashSet::from(["yify".to_owned()]),
                or_title_patterns: Vec::new(),
                and_specs: Vec::new(),
            }],
            tier_bonuses: HashMap::new(),
        };

        assert_eq!(fs.check_blocklist("Movie.2024", Some("YIFY")), Some("lq"));
        assert!(fs.check_blocklist("Movie.2024", Some("NTb")).is_none());
    }

    #[test]
    fn test_tier_bonus() {
        let fs = TrashFilterSet {
            blocklist: Vec::new(),
            tier_bonuses: HashMap::from([("ntb".to_owned(), 0.30), ("flux".to_owned(), 0.20)]),
        };

        assert!((fs.tier_bonus(Some("NTb")) - 0.30).abs() < f64::EPSILON);
        assert!((fs.tier_bonus(Some("FLUX")) - 0.20).abs() < f64::EPSILON);
        assert!((fs.tier_bonus(Some("Unknown")) - 0.0).abs() < f64::EPSILON);
        assert!((fs.tier_bonus(None) - 0.0).abs() < f64::EPSILON);
    }

    #[test]
    fn test_compile_blocklist_rows() {
        let rows = vec![
            TrashFilterRow {
                category: "lq".to_owned(),
                spec_name: "YIFY".to_owned(),
                filter_type: "release_group".to_owned(),
                pattern: "^(YIFY|YTS)$".to_owned(),
                negate: false,
                required: false,
                tier_bonus: None,
            },
            TrashFilterRow {
                category: "lq".to_owned(),
                spec_name: "Bad Title".to_owned(),
                filter_type: "release_title".to_owned(),
                pattern: r"\b(CAMRip)\b".to_owned(),
                negate: false,
                required: false,
                tier_bonus: None,
            },
        ];

        let fs = compile(&rows);
        assert_eq!(fs.blocklist.len(), 1);
        assert!(fs.check_blocklist("Movie.2024", Some("YIFY")).is_some());
        assert!(fs.check_blocklist("Movie.CAMRip.2024", None).is_some());
        assert!(
            fs.check_blocklist("Movie.BluRay.2024", Some("NTb"))
                .is_none()
        );
    }

    #[test]
    fn test_compile_tier_rows() {
        let rows = vec![
            TrashFilterRow {
                category: "web_tier".to_owned(),
                spec_name: "NTb".to_owned(),
                filter_type: "release_group".to_owned(),
                pattern: "^(NTb)$".to_owned(),
                negate: false,
                required: false,
                tier_bonus: Some(0.30),
            },
            TrashFilterRow {
                category: "web_tier".to_owned(),
                spec_name: "NTb-lower".to_owned(),
                filter_type: "release_group".to_owned(),
                pattern: "^(NTb)$".to_owned(),
                negate: false,
                required: false,
                tier_bonus: Some(0.10),
            },
        ];

        let fs = compile(&rows);
        assert!(fs.blocklist.is_empty());
        // Should keep highest bonus.
        assert!((fs.tier_bonus(Some("NTb")) - 0.30).abs() < f64::EPSILON);
    }

    #[test]
    fn test_dedup_rows() {
        let mut rows = vec![
            TrashFilterRow {
                category: "lq".to_owned(),
                spec_name: "YIFY".to_owned(),
                filter_type: "release_group".to_owned(),
                pattern: "^(YIFY)$".to_owned(),
                negate: false,
                required: false,
                tier_bonus: None,
            },
            TrashFilterRow {
                category: "lq".to_owned(),
                spec_name: "YIFY".to_owned(),
                filter_type: "release_group".to_owned(),
                pattern: "^(YIFY)$".to_owned(),
                negate: false,
                required: false,
                tier_bonus: None,
            },
        ];

        dedup_rows(&mut rows);
        assert_eq!(rows.len(), 1);
    }
}
