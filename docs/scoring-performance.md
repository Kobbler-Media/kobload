# RSS scoring performance

How RSS scoring is made faster without changing its decisions. Every
optimisation is checked against a golden snapshot of real production data, and
nothing merges unless it reproduces that snapshot exactly.

## Background

Each watch cycle polls every indexer source in turn and scores every release
against every enabled watch pattern (`watch/service.rs::score_prepared`). In
October 2026 production had 157 enabled patterns, and busy feeds returned
200–1400 releases.

What we found:

- **One source took 6–10 minutes of a pegged core.** A cycle took about 25
  minutes against a 15-minute poll interval. `last_check_at` is written per
  source, so the real interval was the cycle length plus 60 s, and a release
  found by the first source waited up to 25 minutes before winners were picked.
- **The main cost was `has_word` in `quality.rs`.** It compiled a regex on
  almost every call. `parse_quality` makes about 90 such calls, and ran around
  4 times per release × pattern pair, so roughly 20 regex compiles per pair.
  The tokens were also unescaped, so `hdr10+`, `disney+`, `appletv+` and
  `paramount+` could never match.
- **The second cost was running every gate on every pair.** That includes
  quality, about 196 TRaSH regexes, and the structural and sport parses, even
  though nearly every pair is a no-match whose result is discarded.
- **Memory.** Every pair's `ProcessedRelease`, including the full `Release`
  and `raw_data`, was held for koblem review across the whole cycle, even with
  koblem disabled. That pushed API RSS to 3.6–5.6 GB.

Fixed so far:

- `224cbbc`: collect review releases only when koblem is enabled, and
  pre-filter them with `worth_reviewing`.
- `1b327d7`: literal `has_word`; skip full scoring for no-match pairs; reuse
  the quality parse in `score_release`.

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
  `score_release` *without* the match pre-check. This is the unoptimised
  reference, and the optimised loop has to agree with it. It covers the
  decision, every reject reason, every score component and the parsed quality.
- **candidates**: what the real RSS path selected, one best candidate per
  (pattern, episode).
- **review**: what would be queued for koblem if reviews were on.

Floats are compared to 1e-9. Any difference fails the test and is reported
with the fetch, the pattern, the release and the field that changed.

### Determinism

Scoring reads the clock in several places: freshness, air-date gating and pack
recency. These go through `crate::clock::now_utc()`, and replays pin it to each
fetch's capture time.

TRaSH categories used to be compiled from a `HashMap`. That made which category
`check_blocklist` reported, when several matched, vary between restarts. They
are now compiled in a fixed order.

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
builds a second capture directory from real 2026 motorsport releases Kobload
has already seen:

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

### 3. Build the golden snapshot

Copy the capture directory to the dev machine. The first run writes
`golden.jsonl.gz` into it:

```sh
KOBLOAD_SCORING_CORPUS=/path/to/scoring-capture \
  cargo test --release --test scoring_replay_test -- --nocapture
```

### Is the capture ready?

`scripts/scoring-corpus/capture-stats.sh data/scoring-capture` summarises a
capture on the server without building anything. Treat it as complete enough
when all of these hold:

- **Time.** At least 5 days, and ideally the full 7-day window, so it covers a
  weekly TV cycle and at least one F1/MotoGP race weekend (practice,
  qualifying, sprint and race).
- **Sources.** Every RSS-polled source (8 in October 2026; the Cardigann ones
  aren't polled) shows fetches on most
  days, including the 30-minute MyAnonaMouse feed and the busiest Newznab feeds.
- **Busy periods.** Each day has a few thousand new releases. A quiet day
  usually means an indexer was down, so extend the capture.
- **Patterns.** More than one pattern set, meaning wants were added or
  fulfilled during the window, and no fetches without TRaSH rows.
- **Matches** (on the dev machine, from the replay's `--nocapture` output and
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
KOBLOAD_SCORING_CORPUS=… cargo test --release --test scoring_alloc_test -- --nocapture
```

Once a number is worth defending, set `KOBLOAD_SCORING_ALLOC_BUDGET` to make the
allocation count a hard limit.

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

1. **Harness.** Done: capture, replay, golden test, benchmark, allocation
   profile, timing log.
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
     patterns, and run the fuzzy match only on those. That turns 1400 × 157
     comparisons into 1400 × a few, and cost no longer grows with the number of
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
- **Prepare patterns incrementally.** `prepare_patterns` makes several DB round
  trips per pattern per source (about 400 per source). Cache the prepared set
  per (source, pattern `updated_at`), or batch the queries.
- **XEM in RSS scoring.** `build_scoring_config` never calls
  `with_xem_show_map`, so the XEM scene-numbering bonus doesn't apply in RSS.
  That's a behaviour fix, separate from performance, and it will change the
  snapshot.
- **Vectorising.** Not worth hand-rolled SIMD: titles are 50–80 bytes and the
  work is branchy. The SIMD win comes from routing work through
  `memchr`/`aho-corasick`/`regex`, which already use it (item 4).
- **Allocator.** If the remaining heap churn still holds RSS high after item 7,
  try jemalloc or mimalloc with background purging. Compare them using the
  allocation profile and the RSS sampler, not by intuition.
