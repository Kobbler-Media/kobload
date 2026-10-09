-- Indexer capabilities (from Newznab caps endpoint) and rate limit budgeting.
ALTER TABLE indexer_sources
    ADD COLUMN capabilities JSONB DEFAULT NULL,
    ADD COLUMN caps_fetched_at TIMESTAMPTZ DEFAULT NULL,
    ADD COLUMN api_limit_daily INTEGER DEFAULT NULL,
    ADD COLUMN api_calls_today INTEGER NOT NULL DEFAULT 0,
    ADD COLUMN last_api_reset_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    ADD COLUMN search_mode TEXT NOT NULL DEFAULT 'full',
    ADD COLUMN auto_throttle BOOLEAN NOT NULL DEFAULT TRUE;
