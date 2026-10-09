ALTER TABLE downloads
ADD COLUMN IF NOT EXISTS download_client_id UUID REFERENCES download_clients(id);

ALTER TABLE downloads
ADD COLUMN IF NOT EXISTS external_id TEXT;

ALTER TABLE downloads
ADD COLUMN IF NOT EXISTS error TEXT;

ALTER TABLE downloads
ADD COLUMN IF NOT EXISTS callback_sent_at TIMESTAMPTZ;

ALTER TABLE downloads
ADD COLUMN IF NOT EXISTS release_payload JSONB;
