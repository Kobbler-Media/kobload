ALTER TABLE quality_profiles ADD COLUMN reject_remux BOOLEAN NOT NULL DEFAULT FALSE;

-- Enable upsert by (name, media_kind) for reset-to-defaults
CREATE UNIQUE INDEX IF NOT EXISTS quality_profiles_name_media_kind_key
    ON quality_profiles (name, media_kind);
