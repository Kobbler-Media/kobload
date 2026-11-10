# RSS scoring performance

How RSS scoring is made faster without changing its decisions. Every
optimisation is checked against a golden snapshot of captured real-world data,
and nothing merges unless it reproduces that snapshot exactly.

## Background

Each watch cycle polls every indexer source in turn and scores every release
against every enabled watch pattern (`watch/service.rs::score_prepared`). With
a hundred-plus patterns and busy feeds returning 200–1400 releases per poll,
that is hundreds of thousands of release × pattern pairs per cycle.

Where the cost goes:

- **Cycle length.** `last_check_at` is written per source, so the real poll
  interval is the cycle length plus 60 s. A slow source delays every source
  after it, and a release found by the first source waits for the whole cycle
  before winners are picked.
- **Token matching in `quality.rs`.** `parse_quality` makes about 90
  `has_word` probes and runs several times per pair. `has_word` is a literal
  boundary match; it must not compile a regex per call, and tokens containing
  `+` (`hdr10+`, `disney+`) have to match literally.
- **Running every gate on every pair.** Quality, about 196 TRaSH regexes, and
  the structural and sport parses only need to run on pairs that pass the
  match check; nearly every pair is a no-match.
- **Memory.** Releases held for koblem review are collected only when koblem
  is enabled and pre-filtered with `worth_reviewing`; holding every pair's
  `ProcessedRelease` for the whole cycle costs gigabytes of RSS.

## The harness

### What it checks

Scoring is treated as a pure function of three inputs:

- the prepared patterns: each pattern row plus its resolved quality profile,
  match targets, categories, matched GUIDs and preferred protocol
- the TRaSH rule rows
- the releases a source returned

Plus the time, which is pinned with `crate::clock`.

The production loop captures those inputs. The tests then replay them with no
database and compare the result to a golden snapshot. Each replay records:

- **matched**: every release × pattern pair that matched, fully scored by
  `score_release`. Only `release_matches` runs first, which is the same gate
  `score_release` takes its match score from, so nothing is dropped. Scoring
  every non-matching pair too made a full replay take core-hours. This is the
  unoptimised reference, and the optimised loop has to agree with it. It covers the
  decision, every reject reason, every score component and the parsed quality.
- **candidates**: what the real RSS path selected, one best candidate per
  (pattern, episode).
- **review**: what would be queued for koblem if reviews were on.

Fetches replay in parallel, with progress every 100 fetches under
`--nocapture`. A week-long capture takes minutes on a many-core machine.

Floats are compared to 1e-9. Any difference fails the test and is reported
with the fetch, the pattern, the release and the field that changed.

### Determinism

Scoring reads the clock in several places: freshness, air-date gating and pack
recency. These go through `crate::clock::now_utc()`, and replays pin it to each
fetch's capture time.

TRaSH categories are compiled in a fixed order, so the category
`check_blocklist` reports when several match is stable across restarts.

### 1. Capture production data

Capture is opt-in and bounded. Set these on the API container:

| Variable | Default | Meaning |
|---|---|---|
| `KOBLOAD_SCORING_CAPTURE_DIR` | unset (off) | Directory to write into |
| `KOBLOAD_SCORING_CAPTURE_HOURS` | 168 | Capture window, fixed at first start |
| `KOBLOAD_SCORING_CAPTURE_MAX_MB` | 1024 | Stop once the directory reaches this size |

Behaviour:

- Each fetch stores only releases not already captured for that source, since
  feeds repeat most items from one poll to the next.
- Pattern sets and TRaSH rows are stored once per distinct content.
- Expect roughly 10–30 MB a day.
- The window is saved in `capture.json`, so restarts don't extend it.
- When the window or the size limit is reached, capture stops and logs
  "Scoring capture finished".

Example override, using `/data/*`, which is gitignored:

```yaml
  api:
    volumes:
      - ./data/scoring-capture:/data/scoring-capture
    environment:
      KOBLOAD_SCORING_CAPTURE_DIR: /data/scoring-capture
      KOBLOAD_SCORING_CAPTURE_HOURS: "168"
```

The capture contains your watch patterns, so treat it as private: never commit
it.

### 2. Export title corpora (optional)

`scripts/scoring-corpus/export-titles.sh` exports titles for the parser
reference tests. It is read-only against the database and writes to
`data/scoring-corpus/`:

- titles that matched a want
- a 50k sample of DHT names, which carry the foreign, anime, obfuscated and
  scene junk that feeds actually serve

### 2b. Supplementary sport corpus

A week's capture only contains the sport events of that week, while most sport
wants are for past rounds. `scripts/scoring-corpus/build-sport-supplement.py`
builds a second capture directory from motorsport releases Kobload has
already seen:

- releases grabbed for sport wants
- releases matched to sport wants
- F1, MotoGP, Moto2, Moto3 and SBK titles from the DHT hash index

They are scored against the newest captured pattern set, grouped into one
fetch per origin and week, and pinned to that week. The F1 titles have no
matching wants, so they serve as negatives. Session decoys (practice,
qualifying, sprint) exercise the session and round gates.

The script is read-only against the database and strips API keys from stored
URLs. Give it its own golden file by running the replay with
`KOBLOAD_SCORING_CORPUS=data/scoring-capture-sport`.

The main capture still holds indexer download URLs that include API keys, so
copy it only to machines you trust.

### 2c. Production outcomes

`scripts/scoring-corpus/export-labels.sh [capture_dir]` exports what
production actually decided, read-only, into `data/scoring-labels/`:

- `window-grabs.jsonl.gz`: every watch match inside the capture's window and
  what became of it. Use it for the "agreement with production" check.
- `labels.jsonl.gz`: release × want pairs labelled `match`/`no_match`, with how
  the label is known: imported, matched but skipped, rejected, excluded by a
  user, or refused at import. Use it to measure a matcher's precision and
  recall.

Patterns are deleted once they're fulfilled, so labels only cover wants that
still exist. Sport and movies are thin; the sport supplement makes up for
sport.

### 2d. Sport aliases

Captures taken before Kobbler sent place, league and event aliases on sport
wants can't show what those aliases change. `alias_eval_test` gives each captured sport want the aliases Kobbler would
send, rebuilt from the same sources (`api/tests/integration/alias_eval_test/kobbler_aliases.rs`
mirrors Kobbler's `sport_want`), and replays the labels, the sport
supplement and the main capture with and without them. It needs
`data/alias-sources/`:

- `sportarr/{motogp,moto2,moto3,sbk}.json`: Sportarr's season schedules
  (`/api/v2/json/schedule/league/{id}/{season}`)
- `leagues/lg-*.json`: Sportarr's league records, for `alternateNames`
- `pulselive_events.json`: the MotoGP results API's events for the season
- `countries.tsv`, `cities.tsv`: Kobbler's gazetteer
- `adjudication*.tsv`: a verdict for every pair the two disagree on

```sh
KOBLOAD_ALIAS_EVAL_DATA=$PWD/data \
  cargo test --release --test integration alias_eval_test:: -- --nocapture
```

It fails if matching with aliases is worse than without on precision or
recall, or if a disagreement has no verdict, and writes the numbers and
every disagreement to `data/alias-eval/`.

### 3. Build the golden snapshot

Copy the capture directory to your build machine. The first run writes
`golden.jsonl.gz` into it:

```sh
KOBLOAD_SCORING_CORPUS=/path/to/scoring-capture \
  cargo test --release --test integration scoring_replay_test:: -- --nocapture
```

### Is the capture ready?

`scripts/scoring-corpus/capture-stats.sh data/scoring-capture` summarises a
capture where it was taken, without building anything. Treat it as complete enough
when all of these hold:

- **Time.** At least 5 days, and ideally the full 7-day window, so it covers a
  weekly TV cycle and at least one F1/MotoGP race weekend (practice,
  qualifying, sprint and race).
- **Sources.** Every RSS-polled source (Cardigann sources aren't polled)
  shows fetches on most days, including the slowest and busiest feeds.
- **Busy periods.** Each day has a few thousand new releases. A quiet day
  usually means an indexer was down, so extend the capture.
- **Patterns.** More than one pattern set, meaning wants were added or
  fulfilled during the window, and no fetches without TRaSH rows.
- **Matches** (on the build machine, from the replay's `--nocapture` output and
  the golden file):
  - a few hundred matched pairs and dozens of candidates
  - accepts plus several reject reasons: quality, TRaSH blocklist, structural
    mismatch, sport session mismatch, season-pack gates
  - TV episode, TV season, movie and sport wants all represented
- **Agreement with production.** Pick about 10 candidates from the window and
  check what production actually grabbed for those wants. Any disagreement
  should be explained by something outside scoring, such as already
  downloaded, an upgrade cutoff, a hold, or a manual grab.
- **Stable.** After blessing, the replay passes twice in a row.

If one of these is missing, start a new window: delete `capture.json` and the
other files, or point at a new directory. Don't stretch the old window.

Keep that directory and its golden file as the reference for all of the work
below.

### 4. Measure

```sh
# timings
KOBLOAD_SCORING_CORPUS=… cargo bench --bench scoring
# small-device approximation: one core, 256 MB
KOBLOAD_SCORING_CORPUS=… systemd-run --user --scope -p MemoryMax=256M \
  -p AllowedCPUs=0 cargo bench --bench scoring
# allocations: stable across runs, unlike timings
KOBLOAD_SCORING_CORPUS=… cargo bench --bench scoring_alloc
```

A pass over a full week's capture takes on the order of 15 minutes on one
core. `KOBLOAD_SCORING_STEP=10` makes both benches use every 10th fetch, which
keeps the mix of sources and times. Compare numbers taken with the same step
only.

Once a number is worth defending, set `KOBLOAD_SCORING_ALLOC_BUDGET` to make the
allocation count a hard limit. A budget only means something for a given
capture and step, so record both alongside it.

In production, each source logs `Scored source releases` with `pairs`,
`fully_scored`, `prepare_ms` and `score_ms`.

## Rules for each change

1. Make one optimisation per commit.
2. The golden replay must pass unchanged. If a change is *meant* to alter a
   decision (like the `hdr10+` fix), review every reported difference, re-bless
   with `KOBLOAD_SCORING_BLESS=1` in the same commit, and say so in the commit
   message.
3. When replacing an implementation, keep the old one under `#[cfg(test)]` as
   a reference. Compare old against new on the title corpora plus proptest
   input, including Unicode, `+`, `.`, brackets, and tokens at the start or end
   of a title.
4. Record the benchmark and allocation numbers before and after in the commit
   message.
5. For anything that can drop a candidate (indexing, pre-filters), first run
   both paths in production behind a setting, logging disagreements. Delete
   the old path after a week with no diffs.

## Plan

In rough order of risk against payoff.

1. **Harness.** Capture, replay, golden test, benchmark, allocation profile,
   timing log. In place.
2. **Do release-only work once per release, and normalise targets once per
   pattern.** The match check still repeats work per pair that depends only on
   the release: lowercasing, `RELEASE_GROUP_RE`, and `canonical()` per target
   in `watch/matcher.rs`. Hoisting it into a per-release prepared title should
   leave the snapshot byte-identical and cut the remaining per-pair cost by
   about 3–5×.
3. **Parse each release once.** Build a `PreparedRelease` holding:
   - `ParsedQuality`
   - one `ParsedRelease` per media kind (tv, sports, movies, ebooks)
   - the TRaSH blocklist result and tier bonus
   - the episode key

   Then add `score_quality_parsed`, `structural_*_with_parsed` for the sport
   kinds, and `extract_episode_key` taking the parsed TV result. This matters
   mostly for matched pairs and decision-log runs.
4. **One pass for all tokens.**
   - Replace the ~90 sequential `has_word` probes in `parse_quality` with one
     Aho-Corasick automaton (SIMD-accelerated) plus a boundary check on each
     hit.
   - Put the ~180 TRaSH title rules without lookarounds into a `RegexSet`;
     only the 16 with lookarounds need fancy_regex separately.
5. **Skip what's already been scored.** The Newznab RSS returns the page that
   overlaps last cycle (`newznab_rss.rs`). Skip GUIDs scored last cycle when
   the patterns haven't changed since, saving up to ~100 releases per source.
6. **Inverted index of pattern tokens (the big one).**
   - Map each distinctive word in a pattern's targets and aliases to the
     patterns that use it.
   - Split each release title into words once, look up its candidate
     patterns, and run the fuzzy match only on those. That turns releases ×
     all patterns into releases × a few, and cost no longer grows with the number of
     wants.
   - Patterns without a distinctive word fall back to a full scan.
   - Typos and alias spellings must still reach their pattern. The check is
     "the index offers a superset of the patterns the full matcher accepts",
     run over every corpus pair plus proptest typo and alias variants. Then
     shadow mode in production (rule 5) before removing the scan.
7. **Memory for small devices.**
   - Stop cloning the full `Release`, including `raw_data`, into every
     `ProcessedRelease`: share it (`Arc<Release>`, or a reference with a
     lifetime).
   - Drop `raw_data` from the scoring path entirely.
   - dhat shows the before and after; set the allocation budget afterwards.

   `ProcessedRelease` now holds an `Arc<Release>` shared from the fetch.
   `raw_data` stays on `Release`, because matches, holds and downloads
   forward it, but scoring doesn't copy it.

## Further ideas

- **Parallelism.** Scoring is embarrassingly parallel: patterns or release
  chunks under rayon, with a sequential merge into `candidates`. It is cheap to
  add, but on a small box it competes with Postgres, so do it after the
  algorithmic work. Make it opt-in or size-aware.
- **Pick winners per source.** Selection runs at the end of the cycle, so a
  release from the first source waits for every other source. Selecting per
  source, or after a time budget, would cut grab latency. This changes
  behaviour, so it needs its own golden review.
- **Correct poll scheduling.** Base `last_check_at` on when the poll started,
  or schedule sources independently, so that a slow source doesn't delay the
  others.
- **Prepare patterns incrementally.** Patterns are prepared once per watch
  cycle, per (id, `updated_at`), and only categories per source. Carrying the
  prepared set across cycles, or batching the matched-GUID query, would cut
  the remaining ~1.8 statements per pattern per cycle.
- **Vectorising.** Not worth hand-rolled SIMD: titles are 50–80 bytes and the
  work is branchy. The SIMD win comes from routing work through
  `memchr`/`aho-corasick`/`regex`, which already use it (item 4).
- **Allocator.** If the remaining heap churn still holds RSS high after item 7,
  try jemalloc or mimalloc with background purging. Compare them using the
  allocation profile and the RSS sampler, not by intuition.
