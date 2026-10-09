-- Track import state directly on watch_matches so it survives download deletion.
-- When a user deletes a completed download, ON DELETE SET NULL nullifies download_id,
-- breaking all JOINs to downloads for import state. This column preserves that state.

ALTER TABLE watch_matches ADD COLUMN import_confirmed_at TIMESTAMPTZ;

-- Backfill from existing downloads that have been imported
UPDATE watch_matches wm
SET import_confirmed_at = d.import_confirmed_at
FROM downloads d
WHERE wm.download_id = d.id
  AND d.import_confirmed_at IS NOT NULL;
