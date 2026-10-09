CREATE TABLE IF NOT EXISTS prowlarr_config (
    id INTEGER PRIMARY KEY DEFAULT 1,
    base_url TEXT,
    api_key TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

INSERT INTO prowlarr_config (id)
VALUES (1)
ON CONFLICT DO NOTHING;

CREATE TABLE IF NOT EXISTS download_clients (
    id UUID PRIMARY KEY,
    name TEXT NOT NULL,
    client_type TEXT NOT NULL,
    protocol TEXT NOT NULL,
    base_url TEXT NOT NULL,
    api_key TEXT,
    username TEXT,
    password TEXT,
    category TEXT,
    download_dir TEXT,
    is_default BOOLEAN NOT NULL DEFAULT FALSE,
    delete_after_import BOOLEAN NOT NULL DEFAULT FALSE,
    seed_ratio DOUBLE PRECISION,
    seed_time_minutes INTEGER,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS download_clients_enabled_idx ON download_clients (enabled);
CREATE INDEX IF NOT EXISTS download_clients_protocol_idx ON download_clients (protocol);
