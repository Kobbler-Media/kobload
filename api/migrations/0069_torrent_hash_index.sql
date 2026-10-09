-- Local torrent hash index. Maps info_hash → content metadata.
-- Seeded from bulk dumps (RARBG, MagnetDB), updated continuously from
-- DHT crawler, RSS feeds, live search results, and external APIs.
CREATE TABLE torrent_hash_index (
    info_hash TEXT PRIMARY KEY,              -- lowercase hex hash
    imdb_id TEXT,                             -- tt1234567
    tmdb_id TEXT,
    original_title TEXT NOT NULL,             -- raw release title as-is from source (never modified)
    parsed_title TEXT,                        -- cleaned title from parse_release() (can be re-generated)
    year SMALLINT,
    quality JSONB,                            -- parsed quality (resolution, codec, source, audio, group)
    size BIGINT,                              -- file size in bytes
    magnet_url TEXT,
    source TEXT NOT NULL DEFAULT 'unknown',   -- rarbg_dump, magnetdb, tpb, dmm, zilean, torrentio, yts, eztv, dht, rss, live_search
    seeders INT,
    first_seen TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_seen TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_hash_index_imdb ON torrent_hash_index(imdb_id) WHERE imdb_id IS NOT NULL;
CREATE INDEX idx_hash_index_tmdb ON torrent_hash_index(tmdb_id) WHERE tmdb_id IS NOT NULL;
CREATE INDEX idx_hash_index_title ON torrent_hash_index USING gin(to_tsvector('english', original_title));
CREATE INDEX idx_hash_index_source ON torrent_hash_index(source);
