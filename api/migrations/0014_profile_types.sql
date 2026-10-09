CREATE EXTENSION IF NOT EXISTS "pgcrypto";

CREATE TABLE IF NOT EXISTS profile_types (
    id UUID PRIMARY KEY,
    key TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    description TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

ALTER TABLE download_profiles
    ADD COLUMN IF NOT EXISTS profile_type_id UUID;

INSERT INTO profile_types (id, key, name, description, created_at, updated_at)
SELECT gen_random_uuid(), key, initcap(replace(key, '_', ' ')), NULL, NOW(), NOW()
FROM (
    SELECT DISTINCT split_part(profile_type, '-', 1) AS key
    FROM download_profiles
    WHERE profile_type IS NOT NULL
) keys
ON CONFLICT (key) DO NOTHING;

UPDATE download_profiles dp
SET profile_type_id = pt.id
FROM profile_types pt
WHERE pt.key = split_part(dp.profile_type, '-', 1)
  AND dp.profile_type_id IS NULL;

ALTER TABLE download_profiles
    ALTER COLUMN profile_type_id SET NOT NULL;

ALTER TABLE download_profiles
    DROP COLUMN IF EXISTS profile_type;

CREATE INDEX IF NOT EXISTS download_profiles_type_id_idx ON download_profiles (profile_type_id);
