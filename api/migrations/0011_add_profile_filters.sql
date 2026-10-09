ALTER TABLE download_profiles
    ADD COLUMN IF NOT EXISTS profile_filters JSONB;
