CREATE TABLE IF NOT EXISTS profile_parents (
    parent_id UUID NOT NULL REFERENCES download_profiles(id) ON DELETE CASCADE,
    child_id UUID NOT NULL REFERENCES download_profiles(id) ON DELETE CASCADE,
    PRIMARY KEY (parent_id, child_id)
);

CREATE INDEX IF NOT EXISTS profile_parents_parent_idx ON profile_parents (parent_id);
CREATE INDEX IF NOT EXISTS profile_parents_child_idx ON profile_parents (child_id);

DO $$
BEGIN
    IF EXISTS (
        SELECT 1
        FROM information_schema.columns
        WHERE table_name = 'download_profiles'
          AND column_name = 'parent_profile_id'
    ) THEN
        INSERT INTO profile_parents (parent_id, child_id)
        SELECT parent_profile_id, id
        FROM download_profiles
        WHERE parent_profile_id IS NOT NULL
        ON CONFLICT DO NOTHING;

        ALTER TABLE download_profiles
            DROP COLUMN parent_profile_id;
    END IF;
END $$;
