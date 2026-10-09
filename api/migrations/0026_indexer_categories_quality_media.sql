-- Add media_want to watch patterns
ALTER TABLE watch_patterns
    ADD COLUMN IF NOT EXISTS media_want JSONB;

-- Quality profiles for media-aware scoring
CREATE TABLE IF NOT EXISTS quality_profiles (
    id UUID PRIMARY KEY,
    name TEXT NOT NULL,
    media_kind TEXT NOT NULL,
    min_resolution TEXT,
    max_resolution TEXT,
    preferred_sources TEXT[] NOT NULL DEFAULT '{}',
    preferred_codecs TEXT[] NOT NULL DEFAULT '{}',
    preferred_audio TEXT[] NOT NULL DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS quality_profiles_media_kind_idx
    ON quality_profiles (media_kind);

-- Indexer category mapping (generic category key -> indexer-specific category id)
CREATE TABLE IF NOT EXISTS indexer_category_mappings (
    id UUID PRIMARY KEY,
    indexer_source_id UUID NOT NULL REFERENCES indexer_sources(id) ON DELETE CASCADE,
    category_key TEXT NOT NULL,
    indexer_category_id INT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE (indexer_source_id, category_key, indexer_category_id)
);

CREATE INDEX IF NOT EXISTS indexer_category_mappings_source_idx
    ON indexer_category_mappings (indexer_source_id);

CREATE INDEX IF NOT EXISTS indexer_category_mappings_category_idx
    ON indexer_category_mappings (category_key);

-- RSS cache for indexers (raw XML)
CREATE TABLE IF NOT EXISTS indexer_rss_cache (
    indexer_source_id UUID PRIMARY KEY REFERENCES indexer_sources(id) ON DELETE CASCADE,
    fetched_at TIMESTAMPTZ NOT NULL,
    ttl_seconds INT NOT NULL,
    response_xml TEXT NOT NULL
);
