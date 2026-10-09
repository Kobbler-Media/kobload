ALTER TABLE download_profiles
    ADD COLUMN IF NOT EXISTS parent_profile_id UUID REFERENCES download_profiles(id);

UPDATE download_profiles child
SET parent_profile_id = parent.id
FROM download_profiles parent
WHERE child.parent_profile_id IS NULL
  AND child.profile_type LIKE '%-%'
  AND parent.profile_type = split_part(child.profile_type, '-', 1);
