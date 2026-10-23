-- Every release the RSS poll saw, kept for two weeks so the review queue can
-- show what was out there for a want that got nothing.
CREATE TABLE release_sightings (
    release_key TEXT PRIMARY KEY,
    title TEXT NOT NULL,
    size BIGINT,
    published_at TIMESTAMPTZ,
    indexer_source_id UUID,
    indexer TEXT NOT NULL,
    protocol TEXT NOT NULL,
    download_url TEXT,
    magnet_url TEXT,
    info_hash TEXT,
    seen_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX release_sightings_seen_at_idx ON release_sightings (seen_at);
