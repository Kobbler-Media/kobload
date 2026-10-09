-- Track callback retry attempts with exponential backoff
-- Instead of retrying forever every 15 seconds, we:
-- 1. Track attempt count and last attempt time
-- 2. Use exponential backoff: 1min, 5min, 15min, 60min, then give up (5 attempts max)
-- 3. Store last callback error for debugging

ALTER TABLE downloads ADD COLUMN IF NOT EXISTS callback_attempts INTEGER NOT NULL DEFAULT 0;
ALTER TABLE downloads ADD COLUMN IF NOT EXISTS callback_last_attempt_at TIMESTAMPTZ;
ALTER TABLE downloads ADD COLUMN IF NOT EXISTS callback_error TEXT;

-- Track if files couldn't be scanned (for remote download clients where Kobload
-- can't access the storage path). This is a recoverable error - user may need to
-- mount network share, wait for NFS, etc.
-- NULL = not checked yet, empty string = scanned OK, non-empty = error message
ALTER TABLE downloads ADD COLUMN IF NOT EXISTS files_scan_error TEXT;

-- Store raw download client response for debugging
-- Shows what Transmission/SABnzbd/etc told us about the download
ALTER TABLE downloads ADD COLUMN IF NOT EXISTS client_data JSONB;
