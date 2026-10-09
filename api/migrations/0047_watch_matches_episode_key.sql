-- Per-episode tracking for TvSeason watch patterns.
-- NULL for non-season patterns (backwards compatible).
ALTER TABLE watch_matches ADD COLUMN episode_key TEXT;

-- Partial index for per-episode queries on season patterns.
CREATE INDEX watch_matches_episode_key_idx
    ON watch_matches (watch_pattern_id, episode_key)
    WHERE episode_key IS NOT NULL;
