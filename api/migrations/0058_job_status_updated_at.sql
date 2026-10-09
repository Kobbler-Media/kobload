-- Add updated_at column to job_status for orphaned job detection.
-- The worker heartbeat and DB methods set updated_at explicitly;
-- reset_orphaned_running_jobs uses it to find stale "running" rows.

ALTER TABLE job_status
    ADD COLUMN updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW();

-- Backfill: use the most recent known timestamp for existing rows
UPDATE job_status SET updated_at = COALESCE(completed_at, started_at, created_at);
