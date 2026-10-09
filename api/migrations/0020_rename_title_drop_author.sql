-- Rename title to name (more generic) and drop book-centric author column
ALTER TABLE downloads RENAME COLUMN title TO name;
ALTER TABLE downloads DROP COLUMN IF EXISTS author;

-- Track which API key initiated the download (for display purposes)
ALTER TABLE downloads ADD COLUMN IF NOT EXISTS api_key_id UUID REFERENCES api_keys(id) ON DELETE SET NULL;
