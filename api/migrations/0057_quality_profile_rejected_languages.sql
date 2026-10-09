ALTER TABLE quality_profiles
    ADD COLUMN IF NOT EXISTS rejected_languages TEXT[] NOT NULL DEFAULT '{}';

-- Backfill: existing video profiles with no language preference get English preferred
UPDATE quality_profiles
SET preferred_languages = '{english}'
WHERE preferred_languages = '{}'
  AND media_kind IN ('tv', 'movie', 'sports');
