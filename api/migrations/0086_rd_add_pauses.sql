-- Real-Debrid answers error 35 both for a name it refuses and, far more often, while it
-- pauses an account's adds. Refusals recorded before kobload told the two apart are mostly
-- pauses, so the verdicts expire (keeping the file sets stored beside them), and the name
-- risk learned from them and the failures they caused go.
UPDATE debrid_availability_cache SET checked_at = 'epoch' WHERE reason = 'blocked';

UPDATE stream_trait_outcomes SET failure_weight = 0;

UPDATE stream_release_outcomes
SET failures = 0, last_failure_at = NULL, last_failure_reason = NULL
WHERE last_failure_reason ILIKE ANY (ARRAY['%blocked%', '%refused%', '%infringing%']);

-- Torrents streams added, so an account out of active slots can make room by deleting
-- the ones nobody came back for. Not backed up: it describes the account as it is now.
CREATE TABLE debrid_stream_torrents (
    provider_id UUID NOT NULL REFERENCES debrid_providers(id) ON DELETE CASCADE,
    torrent_id TEXT NOT NULL,
    info_hash TEXT NOT NULL,
    touched_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (provider_id, torrent_id)
);
CREATE INDEX idx_debrid_stream_torrents_touched ON debrid_stream_torrents(provider_id, touched_at);
