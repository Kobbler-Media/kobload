-- The debrid log groups stream attempts by the search that listed the release, so it can
-- show the ranked options next to what was tried. Kept as long as the attempts (a day).
CREATE TABLE stream_searches (
    id UUID PRIMARY KEY,
    title TEXT NOT NULL,
    options JSONB NOT NULL DEFAULT '[]',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_stream_searches_created ON stream_searches (created_at DESC);

ALTER TABLE stream_attempts
    ADD COLUMN search_id UUID REFERENCES stream_searches(id) ON DELETE SET NULL,
    ADD COLUMN speed BIGINT;

CREATE INDEX idx_stream_attempts_search ON stream_attempts (search_id) WHERE search_id IS NOT NULL;
