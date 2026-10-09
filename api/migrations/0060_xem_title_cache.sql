CREATE TABLE xem_title_cache (
    tvdb_id BIGINT NOT NULL,
    title TEXT NOT NULL,
    season INTEGER NOT NULL DEFAULT -1,
    fetched_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (tvdb_id, title, season)
);
