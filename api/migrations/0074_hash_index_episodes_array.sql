-- Replace scalar `episode` with `episodes SMALLINT[]` on both
-- `torrent_hash_index` and `torrent_hash_files`.
--
-- Prior model (0073): `episode` is a single SMALLINT; any title parsed with
-- >1 episodes (e.g. S01E09-12) was collapsed to NULL, which meant "pack or
-- unparsed" — a query for S01E10 could not find it via the parent row.
--
-- New model:
--   * episodes = NULL           → pack, or unparsed (matches any episode)
--   * episodes = '{9,10,11,12}' → explicit list of episodes present
-- Search uses `$ep = ANY(episodes)`.
--
-- Backfill is handled by the standalone bin `backfill-hash-index-se`.

ALTER TABLE torrent_hash_index ADD COLUMN episodes SMALLINT[];
ALTER TABLE torrent_hash_files ADD COLUMN episodes SMALLINT[];

DROP INDEX IF EXISTS idx_hash_index_imdb_se;
DROP INDEX IF EXISTS idx_thf_episode;

ALTER TABLE torrent_hash_index DROP COLUMN episode;
ALTER TABLE torrent_hash_files DROP COLUMN episode;

CREATE INDEX idx_hash_index_imdb_season ON torrent_hash_index(imdb_id, season)
    WHERE imdb_id IS NOT NULL;

CREATE INDEX idx_hash_index_episodes_gin ON torrent_hash_index USING gin(episodes)
    WHERE episodes IS NOT NULL;

CREATE INDEX idx_thf_season ON torrent_hash_files(info_hash, season)
    WHERE media_type = 'video';

CREATE INDEX idx_thf_episodes_gin ON torrent_hash_files USING gin(episodes)
    WHERE episodes IS NOT NULL;
