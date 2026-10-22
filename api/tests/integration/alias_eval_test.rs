//! Precision and recall of sport matching with and without the aliases
//! Kobbler now sends, over the labelled set and the captures.
//!
//! The captured wants predate the alias fields, so each sport want gets the
//! fields Kobbler would now add (rebuilt from Sportarr, the MotoGP results
//! API and the gazetteer, see `kobbler_aliases`), and both versions are run
//! through the replay. Skipped unless `KOBLOAD_ALIAS_EVAL_DATA` points at
//! Kobload's `data/` directory with an `alias-sources/` directory in it:
//!
//!   KOBLOAD_ALIAS_EVAL_DATA=$PWD/data \
//!     cargo test --release --test integration alias_eval_test:: -- --nocapture
//!
//! Every pair where the two disagree needs a verdict in
//! `alias-sources/adjudication*.tsv` (want id, release title, 1/0, why);
//! the run lists the ones without, and writes every disagreement to
//! `alias-eval/disagreements.tsv`.

mod kobbler_aliases;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use flate2::read::MultiGzDecoder;
use kobload_api::watch::capture::read_capture;
use kobload_api::watch::{PreparedPattern, Release, replay_scoring};
use rayon::prelude::*;
use serde_json::Value;

use kobbler_aliases::{Sources, meta_for};

const NOT_IDENTITY: &[&str] = &[
    "pack_rejected_low_episode_ratio",
    "pack_rejected_by_preference",
    "episode_want_pack_rejected",
];

#[derive(Clone)]
struct Row {
    corpus: &'static str,
    want_id: String,
    want: String,
    release: String,
    current: (bool, String),
    aliased: (bool, String),
    matched_by: String,
}

fn verdict(entry: Option<&Value>) -> (bool, String) {
    match entry {
        None => (false, "no_match".into()),
        Some(m) => {
            let reason = m["structural_reject_reason"].as_str();
            (
                reason.is_none_or(|r| NOT_IDENTITY.contains(&r)),
                reason
                    .unwrap_or(m["decision"].as_str().unwrap_or("?"))
                    .into(),
            )
        }
    }
}

fn want_label(w: &Value) -> String {
    format!(
        "{} {} R{} {} / {}",
        w["league_name"].as_str().unwrap_or("?"),
        w["season"].as_str().unwrap_or("?"),
        w["round"].as_i64().unwrap_or(0),
        w["location"].as_str().unwrap_or("-"),
        w["session_type"].as_str().unwrap_or("-")
    )
}

fn is_sport(p: &PreparedPattern) -> bool {
    serde_json::to_value(p).unwrap()["pattern"]["media_want"]["kind"] == "sport_event"
}

fn aliased(p: &PreparedPattern, sources: &Sources) -> PreparedPattern {
    let mut v = serde_json::to_value(p).unwrap();
    if let Some(meta) = meta_for(sources, &v["pattern"]["media_want"]) {
        for key in ["/pattern/media_want", "/parsed_media_want"] {
            let target = v.pointer_mut(key).unwrap();
            for (k, val) in meta.as_object().unwrap() {
                if !val.is_null() {
                    target[k] = val.clone();
                }
            }
        }
    }
    serde_json::from_value(v).unwrap()
}

fn index(matched: &[Value]) -> HashMap<(String, String), Value> {
    matched
        .iter()
        .map(|m| {
            (
                (
                    m["pattern_id"].as_str().unwrap().to_string(),
                    m["release"].as_str().unwrap().to_string(),
                ),
                m.clone(),
            )
        })
        .collect()
}

type PatternVersions = (Vec<PreparedPattern>, Vec<PreparedPattern>);

fn capture_rows(dir: &Path, corpus: &'static str, sources: &Sources) -> (Vec<Row>, usize) {
    let fetches = read_capture(dir).expect("read capture");
    let mut sets: HashMap<String, Arc<PatternVersions>> = HashMap::new();
    for f in &fetches {
        sets.entry(f.fetch.patterns.clone()).or_insert_with(|| {
            let sport: Vec<PreparedPattern> = f
                .prepared
                .iter()
                .filter(|p| is_sport(p))
                .map(|p| serde_json::from_value(serde_json::to_value(p).unwrap()).unwrap())
                .collect();
            let with = sport.iter().map(|p| aliased(p, sources)).collect();
            Arc::new((sport, with))
        });
    }
    let results: Vec<(Vec<Row>, usize)> = fetches
        .par_iter()
        .map(|f| {
            let set = &sets[&f.fetch.patterns];
            let run = |prepared: &[PreparedPattern]| {
                index(
                    &replay_scoring(
                        prepared,
                        &f.fetch.releases,
                        f.fetch.source_id,
                        f.trash.as_deref(),
                        f.fetch.captured_at,
                    )
                    .matched,
                )
            };
            let (current, with) = (run(&set.0), run(&set.1));
            let wants: HashMap<String, Value> = set
                .0
                .iter()
                .map(|p| {
                    let v = serde_json::to_value(p).unwrap();
                    (
                        v["pattern"]["id"].as_str().unwrap().to_string(),
                        v["pattern"]["media_want"].clone(),
                    )
                })
                .collect();
            let titles: HashMap<&str, &str> = f
                .fetch
                .releases
                .iter()
                .map(|r| (r.guid.as_str(), r.title.as_str()))
                .collect();
            let keys: HashSet<&(String, String)> = current.keys().chain(with.keys()).collect();
            let rows: Vec<Row> = keys
                .into_iter()
                .map(|k| Row {
                    corpus,
                    want_id: k.0.clone(),
                    want: want_label(&wants[&k.0]),
                    release: titles[k.1.as_str()].to_string(),
                    current: verdict(current.get(k)),
                    aliased: verdict(with.get(k)),
                    matched_by: with
                        .get(k)
                        .and_then(|m| m["matched_pattern"].as_str())
                        .unwrap_or("")
                        .to_string(),
                })
                .filter(|r| r.current.0 || r.aliased.0)
                .collect();
            let both_reject = set.0.len() * f.fetch.releases.len() - rows.len();
            (rows, both_reject)
        })
        .collect();
    let mut seen = HashSet::new();
    let mut rows = Vec::new();
    let mut tn = 0;
    for (r, n) in results {
        tn += n;
        rows.extend(
            r.into_iter()
                .filter(|r| seen.insert((r.want_id.clone(), r.release.clone()))),
        );
    }
    (rows, tn)
}

fn gz_lines(path: &Path) -> Vec<Value> {
    let mut s = String::new();
    std::io::Read::read_to_string(
        &mut MultiGzDecoder::new(std::fs::File::open(path).unwrap()),
        &mut s,
    )
    .unwrap();
    s.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn label_rows(data: &Path, sources: &Sources) -> Vec<Row> {
    let mut patterns: BTreeMap<String, PreparedPattern> = BTreeMap::new();
    for dir in ["scoring-capture", "scoring-capture-sport"] {
        for f in read_capture(&data.join(dir)).unwrap() {
            for p in f.prepared.iter().filter(|p| is_sport(p)) {
                let mut v = serde_json::to_value(p).unwrap();
                v["matched_guids"] = Value::Array(Vec::new());
                v["categories"] = Value::Array(Vec::new());
                v["pattern"]["excluded_releases"] = Value::Null;
                let id = v["pattern"]["id"].as_str().unwrap().to_string();
                patterns
                    .entry(id)
                    .or_insert_with(|| serde_json::from_value(v).unwrap());
            }
        }
    }
    let wants: Vec<(String, Value, PreparedPattern, PreparedPattern)> = patterns
        .into_iter()
        .map(|(id, p)| {
            let want = serde_json::to_value(&p).unwrap()["pattern"]["media_want"].clone();
            let with = aliased(&p, sources);
            (id, want, p, with)
        })
        .collect();
    let labels: Vec<Value> = gz_lines(&data.join("scoring-labels/labels.jsonl.gz"))
        .into_iter()
        .filter(|l| l["media_want"]["kind"] == "sport_event")
        .collect();
    labels
        .par_iter()
        .enumerate()
        .flat_map_iter(|(i, l)| {
            let title = l["release_title"].as_str().unwrap();
            let created = l["created_at"].as_str().unwrap();
            let at = time::OffsetDateTime::parse(
                created,
                &time::format_description::well_known::Rfc3339,
            )
            .unwrap();
            let release: Arc<Release> = Arc::new(
                serde_json::from_value(serde_json::json!({
                    "guid": format!("label-{i}"), "title": title, "size": 2_000_000_000i64,
                    "indexer": "labels", "protocol": "torrent", "publish_date": created,
                    "category_ids": [], "raw_data": {},
                }))
                .unwrap(),
            );
            let own = l["pattern_id"].as_str().unwrap().to_string();
            let run = |p: &PreparedPattern| {
                verdict(
                    replay_scoring(
                        std::slice::from_ref(p),
                        std::slice::from_ref(&release),
                        uuid::Uuid::nil(),
                        None,
                        at,
                    )
                    .matched
                    .first(),
                )
            };
            wants
                .iter()
                .map(|(id, want, p, with)| {
                    let replay = replay_scoring(
                        std::slice::from_ref(with),
                        std::slice::from_ref(&release),
                        uuid::Uuid::nil(),
                        None,
                        at,
                    );
                    Row {
                        corpus: if *id == own { "labels" } else { "labels-decoy" },
                        want_id: id.clone(),
                        want: want_label(want),
                        release: title.to_string(),
                        current: run(p),
                        aliased: verdict(replay.matched.first()),
                        matched_by: replay
                            .matched
                            .first()
                            .and_then(|m| m["matched_pattern"].as_str())
                            .unwrap_or("")
                            .to_string(),
                    }
                })
                .collect::<Vec<_>>()
        })
        .collect()
}

fn adjudication(sources: &Path) -> HashMap<(String, String), (bool, String)> {
    let mut out = HashMap::new();
    for entry in std::fs::read_dir(sources).unwrap().flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if !(name.starts_with("adjudication") && name.ends_with(".tsv")) {
            continue;
        }
        for l in std::fs::read_to_string(entry.path()).unwrap().lines() {
            let parts: Vec<&str> = l.split('\t').collect();
            if l.starts_with('#') || parts.len() < 3 {
                continue;
            }
            let truth = match parts[2] {
                "1" | "true" => true,
                "0" | "false" => false,
                _ => continue,
            };
            out.insert(
                (parts[0].to_string(), parts[1].to_string()),
                (truth, parts.get(3).unwrap_or(&"").to_string()),
            );
        }
    }
    out
}

#[derive(Default)]
struct Counts {
    tp: usize,
    fp: usize,
    fn_: usize,
    tn: usize,
}

impl Counts {
    fn add(&mut self, predicted: bool, truth: bool) {
        match (predicted, truth) {
            (true, true) => self.tp += 1,
            (true, false) => self.fp += 1,
            (false, true) => self.fn_ += 1,
            (false, false) => self.tn += 1,
        }
    }

    fn precision(&self) -> Option<f64> {
        (self.tp + self.fp > 0).then(|| self.tp as f64 / (self.tp + self.fp) as f64)
    }

    fn recall(&self) -> Option<f64> {
        (self.tp + self.fn_ > 0).then(|| self.tp as f64 / (self.tp + self.fn_) as f64)
    }

    fn line(&self) -> String {
        let f = |v: Option<f64>| v.map_or("  n/a ".into(), |v| format!("{v:.4}"));
        format!(
            "P={} R={} (tp {:5} fp {:3} fn {:3} tn {:8})",
            f(self.precision()),
            f(self.recall()),
            self.tp,
            self.fp,
            self.fn_,
            self.tn
        )
    }
}

#[test]
fn aliases_against_the_labelled_set_and_captures() {
    let Some(data) = std::env::var_os("KOBLOAD_ALIAS_EVAL_DATA").map(PathBuf::from) else {
        eprintln!("KOBLOAD_ALIAS_EVAL_DATA not set; skipping the alias evaluation");
        return;
    };
    let source_dir = data.join("alias-sources");
    let sources = Sources::load(&source_dir);
    let truths = adjudication(&source_dir);

    let mut corpora: Vec<(&str, Vec<Row>, usize)> = Vec::new();
    let labels = label_rows(&data, &sources);
    let (own, decoys): (Vec<Row>, Vec<Row>) =
        labels.into_iter().partition(|r| r.corpus == "labels");
    corpora.push(("labels", own, 0));
    let decoys: Vec<Row> = decoys
        .into_iter()
        .filter(|r| r.current.0 || r.aliased.0)
        .collect();
    corpora.push(("labels-decoy", decoys, 0));
    let (rows, tn) = capture_rows(&data.join("scoring-capture-sport"), "supplement", &sources);
    corpora.push(("supplement", rows, tn));
    let (rows, tn) = capture_rows(&data.join("scoring-capture"), "capture", &sources);
    corpora.push(("capture", rows, tn));

    let out_dir = data.join("alias-eval");
    std::fs::create_dir_all(&out_dir).unwrap();
    let mut report = String::new();
    let mut disagreements =
        String::from("corpus\twant_id\twant\trelease\tcurrent\taliased\ttruth\tnote\tmatched_by\n");
    let mut unjudged = Vec::new();
    for (name, rows, implicit_tn) in &corpora {
        let (mut current, mut with) = (Counts::default(), Counts::default());
        current.tn = *implicit_tn;
        with.tn = *implicit_tn;
        for r in rows {
            let key = (r.want_id.clone(), r.release.clone());
            let (truth, note) = match truths.get(&key) {
                Some((t, why)) => (Some(*t), format!("adjudicated: {why}")),
                None if *name == "labels" => (Some(true), "imported".into()),
                None if *name == "labels-decoy" => {
                    (Some(false), "labelled for another want".into())
                }
                None if r.current.0 == r.aliased.0 => (Some(r.current.0), "agreement".into()),
                None => (None, "unadjudicated".into()),
            };
            if r.current.0 != r.aliased.0 {
                let _ = writeln!(
                    disagreements,
                    "{name}\t{}\t{}\t{}\t{}\t{}\t{}\t{note}\t{}",
                    r.want_id,
                    r.want,
                    r.release,
                    r.current.1,
                    r.aliased.1,
                    truth.map_or("?".into(), |t| u8::from(t).to_string()),
                    r.matched_by
                );
            }
            match truth {
                Some(t) => {
                    current.add(r.current.0, t);
                    with.add(r.aliased.0, t);
                }
                None => unjudged.push(format!(
                    "{name}\t{}\t{}\t{}\tcurrent={} aliased={} [{}]",
                    r.want_id, r.want, r.release, r.current.1, r.aliased.1, r.matched_by
                )),
            }
        }
        let _ = writeln!(
            report,
            "[sport] {name}\n  current: {}\n  aliased: {}",
            current.line(),
            with.line()
        );
        let at_least = |a: Option<f64>, b: Option<f64>| match (a, b) {
            (Some(a), Some(b)) => a + 1e-9 >= b,
            (None, _) | (_, None) => true,
        };
        assert!(
            at_least(with.precision(), current.precision())
                && at_least(with.recall(), current.recall()),
            "{name}: aliased matching is worse than current\n{report}"
        );
    }
    std::fs::write(out_dir.join("metrics.txt"), &report).unwrap();
    std::fs::write(out_dir.join("disagreements.tsv"), &disagreements).unwrap();
    eprintln!("{report}");
    assert!(
        unjudged.is_empty(),
        "{} disagreement(s) need a verdict in alias-sources/adjudication*.tsv:\n{}",
        unjudged.len(),
        unjudged.join("\n")
    );
}
