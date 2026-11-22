-- Streams are keyed by release_key: a torrent's info hash unchanged, or nzb-<sha256> for
-- an imported NZB. Library-download and hash-index tables keep info_hash, as those are
-- genuinely torrent hashes.
ALTER TABLE debrid_availability_cache RENAME COLUMN info_hash TO release_key;

ALTER TABLE stream_release_outcomes RENAME COLUMN info_hash TO release_key;
ALTER TABLE stream_release_outcomes
    RENAME CONSTRAINT stream_release_outcomes_provider_id_info_hash_file_id_key
    TO stream_release_outcomes_provider_id_release_key_file_id_key;
ALTER INDEX idx_stream_release_outcomes_hash RENAME TO idx_stream_release_outcomes_release_key;

ALTER TABLE stream_attempts RENAME COLUMN info_hash TO release_key;
ALTER INDEX idx_stream_attempts_hash RENAME TO idx_stream_attempts_release_key;
