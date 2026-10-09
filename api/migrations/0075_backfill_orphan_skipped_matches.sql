-- Backfill rows orphaned by the pre-fix reserve-before-skip bug.
--
-- Before the Bug A fix, want-pipeline call sites reserved a watch_match row
-- with action='downloaded' BEFORE running the upgrade/library skip check.
-- When the skip path fired, the reserved row was left as-is — visible in
-- the UI as "downloaded" forever, with no download_id and no callback.
--
-- This migration rewrites those rows to action='skipped' so the UI can
-- distinguish "we saw it and decided not to act" from real downloads.
-- We don't delete: the rows are useful history.
--
-- Match shape:
--   action='downloaded' AND download_id IS NULL AND import_confirmed_at IS NULL
-- Confined to rows created before this migration runs to avoid clobbering
-- legitimate in-flight reservations from a freshly-deployed version.
UPDATE watch_matches
SET action = 'skipped',
    rejected = TRUE,
    rejected_at = COALESCE(rejected_at, NOW()),
    rejected_reason = COALESCE(
        rejected_reason,
        'orphaned by reserve-before-check bug (pre-fix)'
    )
WHERE action = 'downloaded'
  AND download_id IS NULL
  AND import_confirmed_at IS NULL
  AND created_at < NOW();
