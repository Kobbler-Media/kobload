-- Store scanned files JSON for callback retries
-- When a download completes, we scan the files and store them here.
-- On callback retry, we use this stored data instead of re-scanning
-- (which may fail if files were moved/deleted or client is offline).
ALTER TABLE downloads ADD COLUMN IF NOT EXISTS files_json JSONB;
