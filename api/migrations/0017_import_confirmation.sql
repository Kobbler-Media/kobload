-- Add import confirmation tracking to downloads
-- import_confirmed_at is set when Kobbler confirms it has ingested the files
-- files_json stores the list of files found in the download (for multi-file support)

ALTER TABLE downloads ADD COLUMN IF NOT EXISTS import_confirmed_at TIMESTAMPTZ;
ALTER TABLE downloads ADD COLUMN IF NOT EXISTS files_json JSONB;
