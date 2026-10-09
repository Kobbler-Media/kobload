-- Store the full API key (not just hash) so it can be displayed later
-- This matches Kobbler's ingest_tokens pattern
ALTER TABLE api_keys ADD COLUMN key TEXT;

-- Existing keys won't have the key column populated - they can be regenerated
