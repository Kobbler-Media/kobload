-- Add is_default support for quality profiles.
-- Only one default per media_kind, enforced by partial unique index.

ALTER TABLE quality_profiles
    ADD COLUMN IF NOT EXISTS is_default BOOLEAN NOT NULL DEFAULT FALSE;

CREATE UNIQUE INDEX quality_profiles_default_per_kind
    ON quality_profiles (media_kind) WHERE is_default = TRUE;
