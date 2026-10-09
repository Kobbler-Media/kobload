-- Migrate prowlarr_config into indexer_sources as handler_type='prowlarr'
INSERT INTO indexer_sources (id, name, handler_type, handler_config, enabled, created_at)
SELECT
    gen_random_uuid(),
    'Prowlarr',
    'prowlarr',
    jsonb_build_object('base_url', base_url, 'api_key', api_key),
    TRUE,
    NOW()
FROM prowlarr_config
WHERE id = 1 AND base_url IS NOT NULL;

DROP TABLE prowlarr_config;

-- Per-indexer seeding overrides (take precedence over download client defaults)
ALTER TABLE indexer_sources
    ADD COLUMN seed_ratio DOUBLE PRECISION,
    ADD COLUMN seed_time_minutes INT;

-- General key-value application settings
CREATE TABLE app_settings (
    key TEXT PRIMARY KEY,
    value JSONB NOT NULL DEFAULT '{}',
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
