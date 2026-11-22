-- Servers the usenet engine connects to. Passwords are stored as plainly as the indexer and
-- download-client keys; only the engine-config endpoint, behind the engine's own key, ever
-- returns them.
CREATE TABLE usenet_servers (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name TEXT NOT NULL,
    host TEXT NOT NULL,
    port INTEGER NOT NULL CHECK (port BETWEEN 1 AND 65535),
    tls BOOLEAN NOT NULL DEFAULT true,
    verify_cert BOOLEAN NOT NULL DEFAULT true,
    ip_preference TEXT NOT NULL DEFAULT 'any' CHECK (ip_preference IN ('any', 'v4', 'v6')),
    connections INTEGER NOT NULL CHECK (connections > 0),
    priority INTEGER NOT NULL DEFAULT 0 CHECK (priority >= 0),
    backbone TEXT,
    retention_days INTEGER CHECK (retention_days > 0),
    expires_on DATE,
    quota_bytes BIGINT CHECK (quota_bytes > 0),
    username TEXT NOT NULL DEFAULT '',
    password TEXT NOT NULL DEFAULT '',
    -- Connections the provider allows this login, summed over every row with the same
    -- host and username.
    account_connection_cap INTEGER CHECK (account_connection_cap > 0),
    enabled BOOLEAN NOT NULL DEFAULT true,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- An import the engine refused (missing articles, retention, password) demotes the listing
-- until it is old news; the release has no release_key to remember it by.
ALTER TABLE usenet_releases
    ADD COLUMN last_failure_at TIMESTAMPTZ,
    ADD COLUMN last_failure TEXT;
