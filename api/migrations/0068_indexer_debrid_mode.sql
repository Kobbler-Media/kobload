-- Per-indexer debrid mode: controls which indexers participate in streaming vs download flows
-- 'never'  = traditional download only, excluded from stream search (default for private trackers)
-- 'always' = debrid only, excluded from traditional download, used for stream search + debrid downloads
-- 'both'   = available for both traditional downloads and debrid streaming
ALTER TABLE indexer_sources ADD COLUMN debrid_mode TEXT NOT NULL DEFAULT 'never';
