-- Index for time-range queries on torrent_hash_index (used by hash index RSS source).
CREATE INDEX IF NOT EXISTS idx_hash_index_last_seen ON torrent_hash_index(last_seen);
