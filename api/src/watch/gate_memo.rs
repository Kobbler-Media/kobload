//! Match-gate answers carried from a source's previous fetch to its next one.
//!
//! RSS feeds return mostly the same releases from one poll to the next, and
//! nearly every release × pattern pair fails the match gate. The gate is a
//! pure function of the release title and the pattern's resolved match config
//! (targets, patterns, mode, min score), so its answer for a title seen last
//! fetch is reused while that config is byte-for-byte unchanged. Everything
//! else in scoring still runs every fetch.

use std::collections::HashMap;
use std::sync::Arc;

use uuid::Uuid;

use super::service::{PreparedPattern, has_match_config};
use super::types::Release;

#[derive(Default)]
pub struct MatchGateMemo {
    sources: HashMap<Uuid, FetchGates>,
}

/// One fetch's answers: a column per distinct match config, two bits per
/// column per title (answer known, answer).
#[derive(Default)]
struct FetchGates {
    columns: HashMap<String, usize>,
    titles: HashMap<String, Vec<u64>>,
}

pub(crate) struct GateReuse<'m> {
    memo: &'m mut MatchGateMemo,
    source_id: Uuid,
    columns: HashMap<String, usize>,
    column: Vec<Option<usize>>,
    previous_column: Vec<Option<usize>>,
    previous_rows: Vec<Option<Vec<u64>>>,
    rows: Vec<Vec<u64>>,
}

impl MatchGateMemo {
    pub(crate) fn begin<'m>(
        &'m mut self,
        source_id: Uuid,
        prepared: &[PreparedPattern],
        releases: &[Arc<Release>],
    ) -> GateReuse<'m> {
        let previous = self.sources.remove(&source_id).unwrap_or_default();
        let mut columns = HashMap::new();
        let mut column = Vec::with_capacity(prepared.len());
        let mut previous_column = Vec::with_capacity(prepared.len());
        for prep in prepared {
            let key = has_match_config(prep)
                .then(|| serde_json::to_string(prep.match_config()).ok())
                .flatten();
            previous_column.push(key.as_ref().and_then(|k| previous.columns.get(k).copied()));
            column.push(key.map(|k| {
                let next = columns.len();
                *columns.entry(k).or_insert(next)
            }));
        }
        let words = columns.len().div_ceil(32);
        let previous_rows = releases
            .iter()
            .map(|r| previous.titles.get(&r.title).cloned())
            .collect();
        GateReuse {
            memo: self,
            source_id,
            columns,
            column,
            previous_column,
            previous_rows,
            rows: vec![vec![0; words]; releases.len()],
        }
    }
}

impl GateReuse<'_> {
    pub(crate) fn known(&self, pattern: usize, release: usize) -> Option<bool> {
        let column = self.previous_column[pattern]?;
        let row = self.previous_rows[release].as_ref()?;
        bit(row, 2 * column).then(|| bit(row, 2 * column + 1))
    }

    pub(crate) fn record(&mut self, pattern: usize, release: usize, matched: bool) {
        if let Some(column) = self.column[pattern] {
            let row = &mut self.rows[release];
            set(row, 2 * column);
            if matched {
                set(row, 2 * column + 1);
            }
        }
    }

    pub(crate) fn finish(self, releases: &[Arc<Release>]) {
        let mut titles: HashMap<String, Vec<u64>> = HashMap::with_capacity(releases.len());
        for (release, row) in releases.iter().zip(self.rows) {
            match titles.get_mut(&release.title) {
                Some(existing) => existing.iter_mut().zip(&row).for_each(|(a, b)| *a |= b),
                None => {
                    titles.insert(release.title.clone(), row);
                }
            }
        }
        self.memo.sources.insert(
            self.source_id,
            FetchGates {
                columns: self.columns,
                titles,
            },
        );
    }
}

fn bit(row: &[u64], i: usize) -> bool {
    row[i / 64] >> (i % 64) & 1 == 1
}

fn set(row: &mut [u64], i: usize) {
    row[i / 64] |= 1 << (i % 64);
}
