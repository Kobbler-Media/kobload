-- Clients see a usenet listing only by ref_key, a digest of (indexer_id, guid): an
-- indexer's guid can be its download URL, API key included, so the raw guid stays here.
ALTER TABLE usenet_releases
    ADD COLUMN ref_key TEXT,
    ADD COLUMN fetch_failed_until TIMESTAMPTZ,
    ADD COLUMN fetch_failure TEXT;

UPDATE usenet_releases
SET ref_key = encode(sha256(convert_to(indexer_id || '|' || guid, 'UTF8')), 'hex');

ALTER TABLE usenet_releases ALTER COLUMN ref_key SET NOT NULL;

CREATE UNIQUE INDEX idx_usenet_releases_ref_key ON usenet_releases(ref_key);

CREATE INDEX idx_usenet_releases_unimported_updated ON usenet_releases(updated_at)
    WHERE release_key IS NULL;
