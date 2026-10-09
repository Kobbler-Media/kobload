-- Store release interpretation from parsing (previously computed inline, never persisted)
ALTER TABLE downloads ADD COLUMN IF NOT EXISTS release_interpretation_json JSONB;
