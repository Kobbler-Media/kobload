-- Backfill default quality profiles for existing databases that lack them.
-- Only sets is_default if no default profile exists for that media_kind.

UPDATE quality_profiles SET is_default = TRUE
WHERE name = 'TV - 1080p' AND media_kind = 'tv' AND is_default = FALSE
  AND NOT EXISTS (SELECT 1 FROM quality_profiles WHERE media_kind = 'tv' AND is_default = TRUE);

UPDATE quality_profiles SET is_default = TRUE
WHERE name = 'Movies - 1080p' AND media_kind = 'movie' AND is_default = FALSE
  AND NOT EXISTS (SELECT 1 FROM quality_profiles WHERE media_kind = 'movie' AND is_default = TRUE);
