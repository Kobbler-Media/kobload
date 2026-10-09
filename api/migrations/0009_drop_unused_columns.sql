ALTER TABLE prowlarr_config
    DROP COLUMN IF EXISTS categories,
    DROP COLUMN IF EXISTS indexer_ids;

ALTER TABLE download_profiles
    DROP COLUMN IF EXISTS indexer_ids;
