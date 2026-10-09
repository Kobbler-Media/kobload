-- Bulletproof enhancements: retry mechanisms, watermarking, backlog search, idempotency

-- Issue 1: Download submission retry
-- When download client is unavailable at submission time, mark as pending_retry instead of failed.
-- Allows automatic retry with exponential backoff.
ALTER TABLE downloads ADD COLUMN IF NOT EXISTS submission_attempts INTEGER NOT NULL DEFAULT 1;
ALTER TABLE downloads ADD COLUMN IF NOT EXISTS submission_last_attempt_at TIMESTAMPTZ DEFAULT NOW();
ALTER TABLE downloads ADD COLUMN IF NOT EXISTS submission_error TEXT;

-- Index for efficiently finding downloads needing submission retry
CREATE INDEX IF NOT EXISTS idx_downloads_pending_retry
ON downloads (status, submission_attempts, submission_last_attempt_at)
WHERE status = 'pending_retry';

-- Issue 3: Backlog search tracking
-- Periodically re-search for patterns that haven't found a match yet.
ALTER TABLE watch_patterns ADD COLUMN IF NOT EXISTS last_backlog_search_at TIMESTAMPTZ;
ALTER TABLE watch_patterns ADD COLUMN IF NOT EXISTS backlog_search_interval_hours INTEGER DEFAULT 24;

-- Issue 4: Watch callback retry
-- Track callback attempts for watch matches (similar to download callbacks).
ALTER TABLE watch_matches ADD COLUMN IF NOT EXISTS callback_attempts INTEGER NOT NULL DEFAULT 0;
ALTER TABLE watch_matches ADD COLUMN IF NOT EXISTS callback_last_attempt_at TIMESTAMPTZ;

-- Index for efficiently finding watch matches needing callback retry
CREATE INDEX IF NOT EXISTS idx_watch_matches_callback_retry
ON watch_matches (callback_sent_at, callback_attempts, callback_last_attempt_at)
WHERE callback_sent_at IS NULL AND callback_error IS NOT NULL;

-- Issue 5: Callback idempotency
-- Generate a unique key on first callback attempt, reuse on retries.
-- Kobbler can use this to deduplicate callbacks.
ALTER TABLE downloads ADD COLUMN IF NOT EXISTS callback_idempotency_key UUID;
