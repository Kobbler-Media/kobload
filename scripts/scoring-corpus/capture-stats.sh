#!/usr/bin/env bash
# Summarise a scoring capture without building anything: window, size, and
# per-source and per-day coverage. Use it to judge whether a capture is complete
# enough to bless (see "Is the capture ready?" in docs/scoring-performance.md).
#
#   scripts/scoring-corpus/capture-stats.sh [data/scoring-capture]
#
# Needs zcat and jq. Read-only.
set -euo pipefail

dir="${1:-data/scoring-capture}"
fetches="$dir/fetches.jsonl.gz"

if [[ ! -f "$dir/capture.json" ]]; then
  echo "no capture.json in $dir; capture hasn't started" >&2
  exit 1
fi

echo "== window"
jq -r '"started \(.started_at)\nuntil   \(.until)\nlimit   \(.max_bytes / 1048576 | floor) MiB"' "$dir/capture.json"
echo "used    $(du -sh "$dir" | cut -f1)"
echo "pattern sets: $(find "$dir/patterns" -name '*.json.gz' 2>/dev/null | wc -l)," \
  "trash sets: $(find "$dir/trash" -name '*.json.gz' 2>/dev/null | wc -l)"

if [[ ! -f "$fetches" ]]; then
  echo "no fetches recorded yet"
  exit 0
fi

# One compact row per fetch; the releases themselves are only counted.
rows=$(zcat "$fetches" | jq -c '{
  t: .captured_at, day: .captured_at[0:10], src: .source_name,
  n: (.releases | length), p: .patterns, trash: (.trash != null)
}')

echo
echo "== totals"
jq -rs '"fetches \(length), releases \(map(.n) | add), days \(map(.day) | unique | length), " +
  "fetches without TRaSH rows \(map(select(.trash | not)) | length)"' <<<"$rows"

echo
echo "== per source (fetches, new releases, first, last)"
jq -rs 'group_by(.src)[] |
  "\(.[0].src)\t\(length)\t\(map(.n) | add)\t\(.[0].t)\t\(.[-1].t)"' <<<"$rows" |
  column -t -s $'\t'

echo
echo "== per day (fetches, new releases, sources seen)"
jq -rs 'group_by(.day)[] |
  "\(.[0].day)\t\(length)\t\(map(.n) | add)\t\(map(.src) | unique | length)"' <<<"$rows" |
  column -t -s $'\t'
