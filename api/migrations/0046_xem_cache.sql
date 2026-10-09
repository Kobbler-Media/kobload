-- XEM scene number mapping cache (survives process restarts)
CREATE TABLE xem_cache (
    tvdb_id BIGINT PRIMARY KEY,
    scene_mappings JSONB NOT NULL,
    fetched_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
