CREATE TABLE scene_mapping_cache (
    tvdb_id INTEGER NOT NULL,
    scene_title TEXT NOT NULL,
    season INTEGER NOT NULL DEFAULT -1,
    filter_regex TEXT,
    PRIMARY KEY (tvdb_id, scene_title, season)
);

CREATE TABLE scene_mapping_meta (
    key TEXT PRIMARY KEY DEFAULT 'last_fetched',
    value_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
