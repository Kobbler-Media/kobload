-- Every debrid stream kobload tried to get playable, for the Debrid Log. Diagnostic
-- only: rows are purged after a day by the cache cleanup job.
CREATE TABLE stream_attempts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    provider_id UUID NOT NULL REFERENCES debrid_providers(id) ON DELETE CASCADE,
    origin TEXT NOT NULL,
    info_hash TEXT NOT NULL,
    file_id INTEGER,
    release_title TEXT,
    file_name TEXT,
    status TEXT NOT NULL DEFAULT 'resolving',
    cached BOOLEAN,
    progress REAL,
    torrent_id TEXT,
    error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    ready_at TIMESTAMPTZ
);

CREATE INDEX idx_stream_attempts_created ON stream_attempts(created_at DESC);
CREATE INDEX idx_stream_attempts_torrent ON stream_attempts(provider_id, torrent_id)
    WHERE torrent_id IS NOT NULL;
CREATE INDEX idx_stream_attempts_hash ON stream_attempts(info_hash, created_at DESC);

CREATE TABLE stream_attempt_events (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    attempt_id UUID NOT NULL REFERENCES stream_attempts(id) ON DELETE CASCADE,
    event_type TEXT NOT NULL,
    message TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT clock_timestamp()
);

CREATE INDEX idx_stream_attempt_events_attempt ON stream_attempt_events(attempt_id, created_at);
