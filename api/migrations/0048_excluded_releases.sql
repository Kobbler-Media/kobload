-- Add excluded_releases to watch_patterns for skipping previously rejected releases.
-- Populated from WantRequest.excluded_releases when pattern is created/updated.

ALTER TABLE watch_patterns ADD COLUMN excluded_releases JSONB;
