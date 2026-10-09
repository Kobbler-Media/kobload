-- Downloads cursor pagination (ORDER BY created_at DESC, id DESC)
CREATE INDEX IF NOT EXISTS idx_downloads_created_id ON downloads (created_at DESC, id DESC);

-- Downloads active status partial index (for bulk clear + polling)
CREATE INDEX IF NOT EXISTS idx_downloads_active ON downloads (status) WHERE status IN ('queued', 'downloading');

-- Indexer sources polling (enabled + handler_type + last_check_at)
CREATE INDEX IF NOT EXISTS idx_indexer_sources_check_due ON indexer_sources (last_check_at) WHERE enabled = TRUE AND handler_type != 'prowlarr';

-- Watch matches cursor pagination
CREATE INDEX IF NOT EXISTS idx_watch_matches_created_id ON watch_matches (created_at DESC, id DESC);

-- Watch patterns cursor pagination
CREATE INDEX IF NOT EXISTS idx_watch_patterns_created_id ON watch_patterns (created_at DESC, id DESC);

-- Watch matches rejected filter
CREATE INDEX IF NOT EXISTS idx_watch_matches_rejected ON watch_matches (rejected) WHERE rejected = TRUE;
