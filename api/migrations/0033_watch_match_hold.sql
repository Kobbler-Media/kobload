-- Hold mechanism for protocol delay: when a lower-priority protocol match is found,
-- hold it until the deadline expires or a higher-priority match supersedes it.
ALTER TABLE watch_matches
    ADD COLUMN hold_until TIMESTAMPTZ,
    ADD COLUMN held BOOLEAN NOT NULL DEFAULT FALSE;

-- Index for efficiently finding held matches that are ready to grab.
CREATE INDEX idx_watch_matches_held ON watch_matches (held, hold_until)
    WHERE held = TRUE;
