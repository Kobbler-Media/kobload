-- Debrid provider configuration (Real-Debrid, AllDebrid, Premiumize, TorBox)
CREATE TABLE debrid_providers (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name TEXT NOT NULL,
    provider_type TEXT NOT NULL,  -- 'real_debrid' | 'alldebrid' | 'premiumize' | 'torbox'
    api_key TEXT NOT NULL,
    enabled BOOLEAN NOT NULL DEFAULT true,
    priority INTEGER NOT NULL DEFAULT 0,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Cache of debrid availability checks (info_hash → cached status per provider)
CREATE TABLE debrid_availability_cache (
    info_hash TEXT NOT NULL,
    provider_id UUID NOT NULL REFERENCES debrid_providers(id) ON DELETE CASCADE,
    available BOOLEAN NOT NULL,
    file_data JSONB,
    checked_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (info_hash, provider_id)
);

CREATE INDEX idx_debrid_availability_cache_checked_at ON debrid_availability_cache(checked_at);
