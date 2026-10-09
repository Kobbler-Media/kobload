-- Parsed season/episode on the parent torrent_hash_index row so IMDB searches
-- can reject wrong-season/wrong-episode torrents at the DB layer instead of
-- dragging every torrent for the show through the scoring pipeline.
--
-- Pack-ness is implicit: season=X, episode=NULL means "pack (or unparsed ep)
-- for season X"; season=X, episode=Y means a single episode; both NULL means
-- a movie or failed parse.
ALTER TABLE torrent_hash_index
    ADD COLUMN season SMALLINT,
    ADD COLUMN episode SMALLINT;

CREATE INDEX idx_hash_index_imdb_se ON torrent_hash_index(imdb_id, season, episode)
    WHERE imdb_id IS NOT NULL;
