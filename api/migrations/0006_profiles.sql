CREATE TABLE IF NOT EXISTS download_profiles (
    id UUID PRIMARY KEY,
    name TEXT NOT NULL,
    profile_type TEXT NOT NULL,
    protocol TEXT,
    profile_filters JSONB,
    download_client_id UUID REFERENCES download_clients(id),
    is_default BOOLEAN NOT NULL DEFAULT FALSE,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS download_profiles_type_idx ON download_profiles (profile_type);
CREATE INDEX IF NOT EXISTS download_profiles_enabled_idx ON download_profiles (enabled);

CREATE TABLE IF NOT EXISTS profile_parents (
    parent_id UUID NOT NULL REFERENCES download_profiles(id) ON DELETE CASCADE,
    child_id UUID NOT NULL REFERENCES download_profiles(id) ON DELETE CASCADE,
    PRIMARY KEY (parent_id, child_id)
);

CREATE INDEX IF NOT EXISTS profile_parents_parent_idx ON profile_parents (parent_id);
CREATE INDEX IF NOT EXISTS profile_parents_child_idx ON profile_parents (child_id);
