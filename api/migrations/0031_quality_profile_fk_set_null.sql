-- Allow quality profile deletion by setting downloads FK to SET NULL
ALTER TABLE downloads
    DROP CONSTRAINT IF EXISTS downloads_quality_profile_id_fkey,
    ADD CONSTRAINT downloads_quality_profile_id_fkey
        FOREIGN KEY (quality_profile_id)
        REFERENCES quality_profiles(id)
        ON DELETE SET NULL;
