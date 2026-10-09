-- Generic watch patterns (synced from consumers like Kobbler)
CREATE TABLE watch_patterns (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),

    -- Source tracking
    source_system TEXT NOT NULL,        -- "kobbler", "other_consumer"
    source_id TEXT NOT NULL,            -- External ID (wanted_item UUID in Kobbler)
    callback_url TEXT NOT NULL,         -- Where to POST match notifications

    -- Generic match criteria
    patterns TEXT[] NOT NULL,           -- Match patterns (searched against release title)
    match_mode TEXT NOT NULL DEFAULT 'fuzzy',  -- 'any', 'all', 'fuzzy'
    min_score DOUBLE PRECISION DEFAULT 0.75,

    -- Profile filtering: can specify types (broad) AND/OR specific profiles (narrow)
    profile_type_ids UUID[] DEFAULT '{}',
    profile_ids UUID[] DEFAULT '{}',
    -- At least one must have entries
    CONSTRAINT profile_required CHECK (
        array_length(profile_type_ids, 1) > 0 OR array_length(profile_ids, 1) > 0
    ),

    -- Behavior
    auto_download BOOLEAN DEFAULT TRUE,
    metadata JSONB,                     -- Opaque metadata passed back in callback

    -- State
    enabled BOOLEAN DEFAULT TRUE,
    created_at TIMESTAMPTZ DEFAULT NOW(),
    updated_at TIMESTAMPTZ DEFAULT NOW(),

    UNIQUE (source_system, source_id)
);

CREATE INDEX watch_patterns_enabled_idx ON watch_patterns (enabled) WHERE enabled;

-- Indexer sources (abstracted - not just RSS)
CREATE TABLE indexer_sources (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name TEXT NOT NULL,

    -- Handler type determines how we get releases
    handler_type TEXT NOT NULL,         -- 'prowlarr_rss', 'webhook', 'prowlarr_search', etc.
    handler_config JSONB NOT NULL,      -- Handler-specific config (URL, interval, etc.)

    -- Filtering
    category_ids INT[],                 -- Categories this source provides
    profile_type_id UUID REFERENCES profile_types(id),

    -- State
    enabled BOOLEAN DEFAULT TRUE,
    last_check_at TIMESTAMPTZ,
    last_error TEXT,
    state JSONB,                        -- Handler-specific state (last seen GUID, etc.)

    created_at TIMESTAMPTZ DEFAULT NOW()
);

-- Match history
CREATE TABLE watch_matches (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    watch_pattern_id UUID REFERENCES watch_patterns(id) ON DELETE CASCADE,
    indexer_source_id UUID REFERENCES indexer_sources(id),

    release_guid TEXT NOT NULL,
    release_title TEXT NOT NULL,
    release_data JSONB NOT NULL,
    match_score DOUBLE PRECISION NOT NULL,
    matched_pattern TEXT,               -- Which pattern matched

    action TEXT NOT NULL,               -- 'downloaded', 'notified', 'below_threshold'
    download_id UUID REFERENCES downloads(id),
    callback_sent_at TIMESTAMPTZ,
    callback_error TEXT,

    created_at TIMESTAMPTZ DEFAULT NOW(),
    UNIQUE (watch_pattern_id, release_guid)
);

CREATE INDEX watch_matches_pattern_id_idx ON watch_matches (watch_pattern_id);
CREATE INDEX watch_matches_created_at_idx ON watch_matches (created_at);
