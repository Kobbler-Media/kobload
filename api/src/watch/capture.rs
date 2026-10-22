//! Opt-in capture of RSS scoring inputs, for replaying the scoring loop offline.
//!
//! Each fetch records the releases a source returned together with the exact
//! prepared patterns and TRaSH rows they were scored against, so a test or
//! benchmark can rerun `score_prepared` without the database and compare the
//! result to a golden snapshot. See `docs/scoring-performance.md`.
//!
//! Enabled by `KOBLOAD_SCORING_CAPTURE_DIR`. Capture stops on its own once the
//! window (`KOBLOAD_SCORING_CAPTURE_HOURS`, default 168) has passed or the
//! directory reaches `KOBLOAD_SCORING_CAPTURE_MAX_MB` (default 1024). The window
//! is stored in the directory, so restarts don't extend it.
//!
//! Layout:
//! - `capture.json` — window and limits
//! - `fetches.jsonl.gz` — one [`CapturedFetch`] per line, one gzip member each
//! - `patterns/<hash>.json.gz` — `Vec<PreparedPattern>`, shared by fetches
//! - `trash/<hash>.json.gz` — `Vec<TrashFilterRow>`, shared by fetches

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use flate2::Compression;
use flate2::read::MultiGzDecoder;
use flate2::write::GzEncoder;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use tracing::{info, warn};
use uuid::Uuid;

use super::service::PreparedPattern;
use super::types::Release;
use crate::repositories::trash_filters::{self as trash_repo, TrashFilterRow};
use crate::repositories::watch::IndexerSourceRow;
use crate::trash_filters::TrashFilterSet;

const FORMAT_VERSION: u32 = 1;
const FETCHES_FILE: &str = "fetches.jsonl.gz";
const META_FILE: &str = "capture.json";
const DEFAULT_HOURS: u64 = 168;
const DEFAULT_MAX_MB: u64 = 1024;

#[derive(Debug, Serialize, Deserialize)]
pub struct CaptureMeta {
    pub version: u32,
    #[serde(with = "time::serde::rfc3339")]
    pub started_at: OffsetDateTime,
    #[serde(with = "time::serde::rfc3339")]
    pub until: OffsetDateTime,
    pub max_bytes: u64,
}

/// One source fetch: the releases it returned and what they were scored against.
#[derive(Debug, Serialize, Deserialize)]
pub struct CapturedFetch {
    #[serde(with = "time::serde::rfc3339")]
    pub captured_at: OffsetDateTime,
    pub source_id: Uuid,
    pub source_name: String,
    pub handler_type: String,
    /// Key of the pattern set under `patterns/`.
    pub patterns: String,
    /// Key of the TRaSH rows under `trash/`, if the filters were loaded.
    pub trash: Option<String>,
    /// Only releases this capture hadn't already recorded for the source; feeds
    /// repeat most of their items from one poll to the next.
    pub releases: Vec<Arc<Release>>,
}

pub struct ScoringCapture {
    dir: PathBuf,
    meta: CaptureMeta,
    used_bytes: u64,
    seen: HashMap<Uuid, HashSet<String>>,
    stored: HashSet<String>,
    finished: bool,
}

impl ScoringCapture {
    pub fn from_env() -> Option<Self> {
        let dir = std::env::var("KOBLOAD_SCORING_CAPTURE_DIR")
            .ok()
            .filter(|d| !d.is_empty())?;
        let hours = env_number("KOBLOAD_SCORING_CAPTURE_HOURS").unwrap_or(DEFAULT_HOURS);
        let max_mb = env_number("KOBLOAD_SCORING_CAPTURE_MAX_MB").unwrap_or(DEFAULT_MAX_MB);
        match Self::open(
            Path::new(&dir),
            time::Duration::hours(hours as i64),
            max_mb * 1024 * 1024,
        ) {
            Ok(capture) => {
                info!(
                    dir = %dir,
                    until = %capture.meta.until,
                    max_mb = capture.meta.max_bytes / (1024 * 1024),
                    used_mb = capture.used_bytes / (1024 * 1024),
                    "Scoring capture enabled"
                );
                Some(capture)
            }
            Err(e) => {
                warn!(
                    dir = %dir,
                    error = %e,
                    "Scoring capture disabled: cannot open directory"
                );
                None
            }
        }
    }

    /// Open (or resume) a capture directory. An existing `capture.json` keeps
    /// its original window and limit.
    pub fn open(dir: &Path, window: time::Duration, max_bytes: u64) -> io::Result<Self> {
        fs::create_dir_all(dir.join("patterns"))?;
        fs::create_dir_all(dir.join("trash"))?;
        let meta_path = dir.join(META_FILE);
        let meta = match fs::read(&meta_path) {
            Ok(bytes) => serde_json::from_slice(&bytes).map_err(io::Error::other)?,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                let now = OffsetDateTime::now_utc();
                let meta = CaptureMeta {
                    version: FORMAT_VERSION,
                    started_at: now,
                    until: now + window,
                    max_bytes,
                };
                let json = serde_json::to_vec_pretty(&meta).map_err(io::Error::other)?;
                fs::write(&meta_path, json)?;
                meta
            }
            Err(e) => return Err(e),
        };
        let stored = existing_blobs(dir)?;
        Ok(Self {
            used_bytes: dir_size(dir)?,
            dir: dir.to_path_buf(),
            meta,
            seen: HashMap::new(),
            stored,
            finished: false,
        })
    }

    pub fn is_active(&self) -> bool {
        !self.finished
    }

    pub fn used_bytes(&self) -> u64 {
        self.used_bytes
    }

    /// Record one fetch. Failures are logged and never affect scoring.
    pub async fn record(
        &mut self,
        pool: &sqlx::PgPool,
        source: &IndexerSourceRow,
        prepared: &[PreparedPattern],
        releases: &[Arc<Release>],
    ) {
        if self.finished || releases.is_empty() {
            return;
        }
        // Sorted so an unchanged rule set hashes to the same blob.
        let trash_rows = match trash_repo::get_all(pool).await {
            Ok(mut rows) if !rows.is_empty() => {
                rows.sort_by(|a, b| {
                    (&a.category, &a.spec_name, &a.filter_type, &a.pattern).cmp(&(
                        &b.category,
                        &b.spec_name,
                        &b.filter_type,
                        &b.pattern,
                    ))
                });
                Some(rows)
            }
            _ => None,
        };
        let now = OffsetDateTime::now_utc();
        crate::cpu_bound::run_cpu_bound(|| {
            self.record_fetch(
                now,
                source.id,
                &source.name,
                &source.handler_type,
                prepared,
                trash_rows.as_deref(),
                releases,
            )
        });
    }

    /// The synchronous part of [`record`](Self::record): enforces the window and
    /// size limit, drops releases already recorded for the source, and writes.
    #[allow(clippy::too_many_arguments)]
    pub fn record_fetch(
        &mut self,
        now: OffsetDateTime,
        source_id: Uuid,
        source_name: &str,
        handler_type: &str,
        prepared: &[PreparedPattern],
        trash_rows: Option<&[TrashFilterRow]>,
        releases: &[Arc<Release>],
    ) {
        if self.finished {
            return;
        }
        if now >= self.meta.until || self.used_bytes >= self.meta.max_bytes {
            self.finish(now);
            return;
        }

        let seen = self.seen.entry(source_id).or_default();
        let new: Vec<Arc<Release>> = releases
            .iter()
            .filter(|r| !seen.contains(&r.guid))
            .cloned()
            .collect();
        if new.is_empty() {
            return;
        }

        let written = self.write_fetch(
            now,
            source_id,
            source_name,
            handler_type,
            prepared,
            trash_rows,
            new,
        );
        match written {
            Ok(guids) => self.seen.entry(source_id).or_default().extend(guids),
            Err(e) => {
                warn!(error = %e, "Scoring capture write failed; stopping capture");
                self.finished = true;
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn write_fetch(
        &mut self,
        now: OffsetDateTime,
        source_id: Uuid,
        source_name: &str,
        handler_type: &str,
        prepared: &[PreparedPattern],
        trash_rows: Option<&[TrashFilterRow]>,
        releases: Vec<Arc<Release>>,
    ) -> io::Result<Vec<String>> {
        let patterns = self.store_blob("patterns", prepared)?;
        let trash = trash_rows
            .map(|rows| self.store_blob("trash", rows))
            .transpose()?;
        let guids = releases.iter().map(|r| r.guid.clone()).collect();
        let fetch = CapturedFetch {
            captured_at: now,
            source_id,
            source_name: source_name.to_string(),
            handler_type: handler_type.to_string(),
            patterns,
            trash,
            releases,
        };
        let mut line = serde_json::to_vec(&fetch).map_err(io::Error::other)?;
        line.push(b'\n');
        let compressed = gzip(&line)?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.dir.join(FETCHES_FILE))?;
        file.write_all(&compressed)?;
        self.used_bytes += compressed.len() as u64;
        Ok(guids)
    }

    /// Write `value` under `kind/` keyed by its content hash, once.
    fn store_blob<T: Serialize + ?Sized>(&mut self, kind: &str, value: &T) -> io::Result<String> {
        let json = serde_json::to_vec(value).map_err(io::Error::other)?;
        let key = const_hex::encode(Sha256::digest(&json))[..16].to_string();
        let stored_key = format!("{kind}/{key}");
        if !self.stored.contains(&stored_key) {
            let compressed = gzip(&json)?;
            fs::write(self.dir.join(format!("{stored_key}.json.gz")), &compressed)?;
            self.used_bytes += compressed.len() as u64;
            self.stored.insert(stored_key);
        }
        Ok(key)
    }

    fn finish(&mut self, now: OffsetDateTime) {
        self.finished = true;
        self.seen = HashMap::new();
        let reason = if now >= self.meta.until {
            "window ended"
        } else {
            "size limit reached"
        };
        info!(
            dir = %self.dir.display(),
            used_mb = self.used_bytes / (1024 * 1024),
            reason,
            "Scoring capture finished"
        );
    }
}

/// A captured fetch with its pattern set and TRaSH filters loaded.
pub struct CorpusFetch {
    pub fetch: CapturedFetch,
    pub prepared: Arc<Vec<PreparedPattern>>,
    pub trash: Option<Arc<TrashFilterSet>>,
}

/// Load every fetch in a capture directory, in capture order.
pub fn read_capture(dir: &Path) -> io::Result<Vec<CorpusFetch>> {
    let mut patterns: HashMap<String, Arc<Vec<PreparedPattern>>> = HashMap::new();
    let mut trash: HashMap<String, Arc<TrashFilterSet>> = HashMap::new();
    let mut out = Vec::new();

    let reader = BufReader::new(MultiGzDecoder::new(File::open(dir.join(FETCHES_FILE))?));
    for line in reader.lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let fetch: CapturedFetch = serde_json::from_str(&line).map_err(io::Error::other)?;
        let prepared = match patterns.get(&fetch.patterns) {
            Some(p) => p.clone(),
            None => {
                let p: Arc<Vec<PreparedPattern>> =
                    Arc::new(read_blob(dir, "patterns", &fetch.patterns)?);
                patterns.insert(fetch.patterns.clone(), p.clone());
                p
            }
        };
        let filters = match &fetch.trash {
            None => None,
            Some(key) => Some(match trash.get(key) {
                Some(t) => t.clone(),
                None => {
                    let rows: Vec<TrashFilterRow> = read_blob(dir, "trash", key)?;
                    let t = Arc::new(TrashFilterSet::from_rows(&rows));
                    trash.insert(key.clone(), t.clone());
                    t
                }
            }),
        };
        out.push(CorpusFetch {
            fetch,
            prepared,
            trash: filters,
        });
    }
    Ok(out)
}

/// Each fetch's releases as its feed served them. A capture keeps only the
/// releases it hadn't seen from the source before, but a feed returns its
/// newest items every poll, so this puts the overlap back: each fetch's new
/// releases followed by the source's previous feed, `page` items in all (or
/// every new release, if there are more).
pub fn overlapping_feeds(corpus: &[CorpusFetch], page: usize) -> Vec<Vec<Arc<Release>>> {
    let mut previous: HashMap<Uuid, Vec<Arc<Release>>> = HashMap::new();
    corpus
        .iter()
        .map(|f| {
            let new = &f.fetch.releases;
            let feed: Vec<Arc<Release>> = new
                .iter()
                .cloned()
                .chain(previous.remove(&f.fetch.source_id).unwrap_or_default())
                .take(page.max(new.len()))
                .collect();
            previous.insert(f.fetch.source_id, feed.clone());
            feed
        })
        .collect()
}

fn read_blob<T: serde::de::DeserializeOwned>(dir: &Path, kind: &str, key: &str) -> io::Result<T> {
    let mut json = Vec::new();
    MultiGzDecoder::new(File::open(dir.join(kind).join(format!("{key}.json.gz")))?)
        .read_to_end(&mut json)?;
    serde_json::from_slice(&json).map_err(io::Error::other)
}

fn gzip(bytes: &[u8]) -> io::Result<Vec<u8>> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(bytes)?;
    encoder.finish()
}

fn env_number(name: &str) -> Option<u64> {
    std::env::var(name).ok()?.trim().parse().ok()
}

fn existing_blobs(dir: &Path) -> io::Result<HashSet<String>> {
    let mut keys = HashSet::new();
    for kind in ["patterns", "trash"] {
        for entry in fs::read_dir(dir.join(kind))? {
            let name = entry?.file_name().to_string_lossy().into_owned();
            if let Some(key) = name.strip_suffix(".json.gz") {
                keys.insert(format!("{kind}/{key}"));
            }
        }
    }
    Ok(keys)
}

fn dir_size(dir: &Path) -> io::Result<u64> {
    let mut total = 0;
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let meta = entry.metadata()?;
        total += if meta.is_dir() {
            dir_size(&entry.path())?
        } else {
            meta.len()
        };
    }
    Ok(total)
}
