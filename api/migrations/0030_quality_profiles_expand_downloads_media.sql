-- Expand quality_profiles with language, rejected sources, hardcoded subs
ALTER TABLE quality_profiles
    ADD COLUMN IF NOT EXISTS preferred_languages TEXT[] NOT NULL DEFAULT '{}',
    ADD COLUMN IF NOT EXISTS rejected_sources TEXT[] NOT NULL DEFAULT '{}',
    ADD COLUMN IF NOT EXISTS reject_hardcoded_subs BOOLEAN NOT NULL DEFAULT false;

-- Add quality_profile_id and media_kind to downloads
ALTER TABLE downloads
    ADD COLUMN IF NOT EXISTS quality_profile_id UUID REFERENCES quality_profiles(id),
    ADD COLUMN IF NOT EXISTS media_kind TEXT;
