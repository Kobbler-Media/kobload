CREATE TABLE review_items (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    watch_pattern_id UUID NOT NULL REFERENCES watch_patterns(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK (kind IN ('missed', 'doubtful')),
    status TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'resolved', 'dismissed')),
    detail JSONB NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    resolved_at TIMESTAMPTZ,
    -- Once per want and kind, whatever became of it, so a dismissed item
    -- never comes back.
    UNIQUE (watch_pattern_id, kind)
);

CREATE INDEX review_items_open_idx ON review_items (created_at DESC) WHERE status = 'open';

CREATE TABLE review_rejections (
    watch_pattern_id UUID NOT NULL REFERENCES watch_patterns(id) ON DELETE CASCADE,
    release_key TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (watch_pattern_id, release_key)
);
