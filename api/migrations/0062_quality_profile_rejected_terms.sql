ALTER TABLE quality_profiles
    ADD COLUMN IF NOT EXISTS rejected_terms TEXT[] NOT NULL DEFAULT '{}';

-- Populate defaults for all existing video (tv/movie) profiles
UPDATE quality_profiles
SET rejected_terms = ARRAY['ASL', 'BSL', '.Signed.', 'Sign.Zone', 'SignZone', '3D', 'HSBS', 'Half-SBS', 'HOU', 'Half-OU']
WHERE media_kind IN ('tv', 'movie')
  AND rejected_terms = '{}';
