ALTER TABLE downloads
    ADD COLUMN IF NOT EXISTS profile_id UUID REFERENCES download_profiles(id);

UPDATE downloads d
SET profile_id = dp.id
FROM download_profiles dp
WHERE d.profile_type IS NOT NULL
  AND d.profile_id IS NULL
  AND dp.name ILIKE '%' || d.profile_type || '%';

ALTER TABLE downloads
    DROP COLUMN IF EXISTS profile_type;
