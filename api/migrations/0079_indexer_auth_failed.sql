-- Separate "your credentials stopped working" from "the indexer is flaky".
--
-- Cookie/session indexers (MAM, TorrentDay) fail permanently once the session
-- dies, but looked identical to a transient outage: consecutive_failures climbed,
-- exponential backoff retried forever, and the card showed a generic last_error.
-- A dedicated timestamp lets the UI say "re-enter the cookie" and stops the
-- retry loop from pretending progress is possible.
ALTER TABLE indexer_sources
    ADD COLUMN auth_failed_at TIMESTAMPTZ;

COMMENT ON COLUMN indexer_sources.auth_failed_at IS
    'When the indexer last rejected our credentials; cleared on the next success. Non-NULL means the config needs a human.';
