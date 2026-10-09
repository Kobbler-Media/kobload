CREATE TABLE trash_filter_cache (
    id SERIAL PRIMARY KEY,
    category TEXT NOT NULL,
    spec_name TEXT NOT NULL,
    filter_type TEXT NOT NULL,
    pattern TEXT NOT NULL,
    negate BOOLEAN NOT NULL DEFAULT FALSE,
    required BOOLEAN NOT NULL DEFAULT FALSE,
    tier_bonus DOUBLE PRECISION
);

CREATE INDEX idx_trash_filter_category ON trash_filter_cache(category);

CREATE TABLE trash_filter_meta (
    key TEXT PRIMARY KEY DEFAULT 'last_fetched',
    value_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
