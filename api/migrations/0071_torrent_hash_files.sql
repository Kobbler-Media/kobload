-- Per-file metadata for multi-file torrents (season packs, etc.)
-- Enables matching individual episodes within pack torrents.
-- Files are classified at ingest time so search can filter by season/episode.
CREATE TABLE torrent_hash_files (
    info_hash  TEXT NOT NULL REFERENCES torrent_hash_index(info_hash) ON DELETE CASCADE,
    file_idx   INT NOT NULL,
    filename   TEXT NOT NULL,
    size       BIGINT,
    season     SMALLINT,        -- parsed from filename, NULL if not detected
    episode    SMALLINT,        -- parsed from filename, NULL if not detected
    media_type TEXT NOT NULL DEFAULT 'video',  -- video, subtitle, sample, other
    PRIMARY KEY (info_hash, file_idx)
);

-- Fast lookup for hash index search (join path)
CREATE INDEX idx_thf_info_hash ON torrent_hash_files(info_hash);

-- Filtered episode lookup: given a torrent, find the file for S01E01
CREATE INDEX idx_thf_episode ON torrent_hash_files(info_hash, season, episode)
    WHERE media_type = 'video';
