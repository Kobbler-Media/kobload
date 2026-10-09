CREATE TABLE IF NOT EXISTS downloads (
    id UUID PRIMARY KEY,
    source TEXT NOT NULL,
    protocol TEXT NOT NULL,
    title TEXT NOT NULL,
    author TEXT,
    download_url TEXT NOT NULL,
    callback_url TEXT NOT NULL,
    auth_token TEXT,
    metadata JSONB NOT NULL DEFAULT '{}'::jsonb,
    status TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS downloads_status_idx ON downloads (status);
