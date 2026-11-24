-- A season's near-air episode is searched on its own 12 h clock, so old gaps keep theirs.
ALTER TABLE watch_patterns ADD COLUMN last_near_air_search_at TIMESTAMPTZ;
