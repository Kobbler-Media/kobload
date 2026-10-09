-- Replace profile system with simpler media_type_config.
-- media_type_config is indexer-agnostic; handler_type_defaults provides
-- per-handler category mappings.

-- 1. Create media_type_config (seeded from profile_types)
CREATE TABLE media_type_config (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    key TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    protocol_priority TEXT[] NOT NULL DEFAULT '{}',
    protocol_delay_minutes INTEGER,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

INSERT INTO media_type_config (id, key, name)
SELECT id, key, name FROM profile_types ON CONFLICT DO NOTHING;

-- 2. Create handler_type_defaults
CREATE TABLE handler_type_defaults (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    handler_type TEXT NOT NULL,
    media_type_id UUID NOT NULL REFERENCES media_type_config(id) ON DELETE CASCADE,
    category_ids TEXT[] NOT NULL DEFAULT '{}',
    UNIQUE(handler_type, media_type_id)
);

INSERT INTO handler_type_defaults (handler_type, media_type_id, category_ids)
SELECT v.handler_type, mtc.id, v.category_ids::TEXT[]
FROM (VALUES
    ('newznab', 'ebooks', '{7020,7000}'),
    ('newznab', 'audiobooks', '{3030,3000}'),
    ('newznab', 'tv', '{5000}'),
    ('newznab', 'movies', '{2000}'),
    ('newznab', 'music', '{3010,3000}'),
    ('newznab', 'comics', '{7030,7000}'),
    ('newznab', 'games', '{1000,4010}'),
    ('newznab', 'software', '{4020,4000}'),
    ('torznab', 'ebooks', '{7020,7000}'),
    ('torznab', 'audiobooks', '{3030,3000}'),
    ('torznab', 'tv', '{5000}'),
    ('torznab', 'movies', '{2000}'),
    ('torznab', 'music', '{3010,3000}'),
    ('torznab', 'comics', '{7030,7000}'),
    ('torznab', 'games', '{1000,4010}'),
    ('torznab', 'software', '{4020,4000}')
) AS v(handler_type, media_key, category_ids)
JOIN media_type_config mtc ON mtc.key = v.media_key
ON CONFLICT DO NOTHING;

-- 3. indexer_sources: profile_type_id -> media_type_ids
ALTER TABLE indexer_sources ADD COLUMN media_type_ids UUID[] NOT NULL DEFAULT '{}';
UPDATE indexer_sources SET media_type_ids = ARRAY[profile_type_id]
    WHERE profile_type_id IS NOT NULL;
ALTER TABLE indexer_sources DROP COLUMN profile_type_id;

-- 4. watch_patterns: profile_type_ids/profile_ids -> media_type_id
ALTER TABLE watch_patterns ADD COLUMN media_type_id UUID REFERENCES media_type_config(id);
UPDATE watch_patterns SET media_type_id = profile_type_ids[1]
    WHERE array_length(profile_type_ids, 1) > 0;
ALTER TABLE watch_patterns DROP CONSTRAINT IF EXISTS profile_required;
ALTER TABLE watch_patterns DROP COLUMN profile_type_ids;
ALTER TABLE watch_patterns DROP COLUMN profile_ids;

-- 5. indexer_category_mappings: category_key -> media_type_id
ALTER TABLE indexer_category_mappings ADD COLUMN media_type_id UUID REFERENCES media_type_config(id) ON DELETE CASCADE;
UPDATE indexer_category_mappings SET media_type_id = (
    SELECT id FROM media_type_config WHERE key = indexer_category_mappings.category_key
);
ALTER TABLE indexer_category_mappings DROP CONSTRAINT IF EXISTS indexer_category_mappings_indexer_source_id_category_key_in_key;
ALTER TABLE indexer_category_mappings DROP COLUMN category_key;
ALTER TABLE indexer_category_mappings ADD UNIQUE (indexer_source_id, media_type_id, indexer_category_id);

-- 6. downloads: make profile_id nullable (profiles table going away)
ALTER TABLE downloads ALTER COLUMN profile_id DROP NOT NULL;

-- 7. Drop old profile tables
DROP TABLE IF EXISTS profile_parents;
DROP TABLE IF EXISTS download_profiles CASCADE;
DROP TABLE IF EXISTS profile_types CASCADE;
