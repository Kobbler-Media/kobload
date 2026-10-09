-- Quality upgrade system: quality-aware RSS matching, upgrade windows, rejection

-- Quality profiles: upgrade cutoff (score above which we're "satisfied")
ALTER TABLE quality_profiles ADD COLUMN upgrade_cutoff_score DOUBLE PRECISION;

-- Watch patterns: per-want upgrade config
ALTER TABLE watch_patterns ADD COLUMN upgrade_allowed BOOLEAN NOT NULL DEFAULT TRUE;
ALTER TABLE watch_patterns ADD COLUMN upgrade_until TIMESTAMPTZ;

-- Watch matches: quality score of the release (from quality.rs scoring, NOT match_score which is title-match quality)
ALTER TABLE watch_matches ADD COLUMN quality_score DOUBLE PRECISION;

-- Watch matches: track rejection for "give me something else"
ALTER TABLE watch_matches ADD COLUMN rejected BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE watch_matches ADD COLUMN rejected_at TIMESTAMPTZ;
ALTER TABLE watch_matches ADD COLUMN rejected_reason TEXT;
