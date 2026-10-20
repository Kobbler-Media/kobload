#!/usr/bin/env bash
# Export what production actually decided, as ground truth for the scoring
# replay and the matcher work. Read-only against the database.
#
#   window-grabs.jsonl.gz  every watch match recorded inside a capture's window,
#                          with what became of it. Use it to check a replay's
#                          candidates agree with production.
#   labels.jsonl.gz        release title x want pairs labelled by outcome, for
#                          precision/recall of a matcher. Each row's `label` is
#                          match/no_match and `evidence` says how sure that is:
#                            imported          grabbed and confirmed imported      match
#                            matched_skipped   matched, skipped for quality/dupes  match (weak)
#                            rejected          match rejected after the fact       no_match
#                            excluded          excluded on the pattern by a user   no_match
#                            import_failed     kobbler refused the import          no_match (weak:
#                                              also covers archives, exe-only packs, etc.)
#
# Usage: scripts/scoring-corpus/export-labels.sh [capture_dir] [out_dir]
#   capture_dir  default data/scoring-capture; its capture.json gives the window
#   out_dir      default data/scoring-labels (gitignored)
#
# Env: DB_CONTAINER (default kobload-db-1), DB_USER/DB_NAME (default kobload).
set -euo pipefail

capture_dir=${1:-data/scoring-capture}
out_dir=${2:-data/scoring-labels}
container=${DB_CONTAINER:-kobload-db-1}
user=${DB_USER:-kobload}
db=${DB_NAME:-kobload}

window=$(python3 -c 'import json,sys; c=json.load(open(sys.argv[1])); print(c["started_at"], c["until"])' \
    "$capture_dir/capture.json")
read -r started until <<<"$window"
mkdir -p "$out_dir"

psql() {
    docker exec -i "$container" psql -U "$user" -d "$db" -v ON_ERROR_STOP=1 -AtX "$@"
}

# GUIDs and titles never carry keys on our indexers, but strip apikey-style
# query parameters anyway so the export is safe to copy around.
strip_keys="regexp_replace(%s, '([?&](apikey|api_key|r|passkey|token)=)[^&]+', '\\1REDACTED', 'gi')"
guid=$(printf "$strip_keys" "m.release_guid")

psql -c "
SELECT row_to_json(t) FROM (
  SELECT m.created_at, m.watch_pattern_id AS pattern_id, s.name AS source,
         $guid AS release_guid, m.release_title, m.match_score, m.quality_score,
         m.action, m.rejected, m.rejected_reason, m.held, m.hold_until,
         m.import_confirmed_at, m.download_id, d.status AS download_status,
         d.media_kind, p.media_want
  FROM watch_matches m
  LEFT JOIN indexer_sources s ON s.id = m.indexer_source_id
  LEFT JOIN downloads d ON d.id = m.download_id
  LEFT JOIN watch_patterns p ON p.id = m.watch_pattern_id
  WHERE m.created_at >= '$started' AND m.created_at < '$until'
  ORDER BY m.created_at
) t" | gzip -9 > "$out_dir/window-grabs.jsonl.gz"

psql -c "
WITH refused AS (
  SELECT DISTINCT download_id FROM download_events
  WHERE event_type IN ('import_failed', 'content_validation_failed')
)
-- A release is matched again every cycle it stays in a feed; keep one row per
-- title x pattern, preferring the strongest evidence.
SELECT row_to_json(t) FROM (
 SELECT DISTINCT ON (release_title, pattern_id) * FROM (
  SELECT m.release_title, p.media_want, d.media_kind, m.watch_pattern_id AS pattern_id,
         CASE WHEN m.rejected AND m.action = 'rejected' THEN 'no_match'
              WHEN r.download_id IS NOT NULL AND m.import_confirmed_at IS NULL THEN 'no_match'
              ELSE 'match' END AS label,
         CASE WHEN m.rejected AND m.action = 'rejected' THEN 'rejected'
              WHEN r.download_id IS NOT NULL AND m.import_confirmed_at IS NULL THEN 'import_failed'
              WHEN m.import_confirmed_at IS NOT NULL THEN 'imported'
              ELSE 'matched_skipped' END AS evidence,
         m.rejected_reason, m.action, m.created_at
  FROM watch_matches m
  JOIN watch_patterns p ON p.id = m.watch_pattern_id
  LEFT JOIN downloads d ON d.id = m.download_id
  LEFT JOIN refused r ON r.download_id = m.download_id
  WHERE m.release_title IS NOT NULL AND m.release_title <> ''
    AND (m.import_confirmed_at IS NOT NULL
         OR (m.rejected AND m.action = 'rejected')
         OR r.download_id IS NOT NULL
         OR m.action LIKE 'skipped%')
  UNION ALL
  SELECT x->>'release_title', p.media_want, NULL, p.id, 'no_match', 'excluded',
         NULL, NULL, p.updated_at
  FROM watch_patterns p, jsonb_array_elements(p.excluded_releases) x
  WHERE jsonb_typeof(p.excluded_releases) = 'array' AND x->>'release_title' IS NOT NULL
 ) u
 ORDER BY release_title, pattern_id,
          array_position(ARRAY['rejected','excluded','import_failed','imported','matched_skipped'], evidence),
          created_at DESC
) t" | gzip -9 > "$out_dir/labels.jsonl.gz"

echo "window $started .. $until"
echo "$out_dir/window-grabs.jsonl.gz: $(zcat "$out_dir/window-grabs.jsonl.gz" | wc -l) matches"
echo "$out_dir/labels.jsonl.gz: $(zcat "$out_dir/labels.jsonl.gz" | wc -l) pairs"
zcat "$out_dir/labels.jsonl.gz" | python3 -c '
import collections, json, sys
c = collections.Counter()
for line in sys.stdin:
    r = json.loads(line)
    c[(r["label"], r["evidence"], (r["media_want"] or {}).get("kind"))] += 1
for k, n in sorted(c.items(), key=lambda kv: -kv[1]):
    print(f"  {n:6}  {k[0]:9} {k[1]:16} {k[2]}")'
