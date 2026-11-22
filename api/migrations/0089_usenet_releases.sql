-- A usenet option is known by the indexer that listed it until its NZB is imported; then
-- it gets a release_key derived from the NZB's content, which aliases listings from
-- different indexers onto one release. download_url carries the indexer's API key and
-- never leaves kobload: clients see only (indexer_id, guid).
CREATE TABLE usenet_releases (
    indexer_id TEXT NOT NULL,
    guid TEXT NOT NULL,
    download_url TEXT NOT NULL,
    release_title TEXT,
    release_key TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (indexer_id, guid)
);

CREATE INDEX idx_usenet_releases_release_key ON usenet_releases(release_key)
    WHERE release_key IS NOT NULL;
