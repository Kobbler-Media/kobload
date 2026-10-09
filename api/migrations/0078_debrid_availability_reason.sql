-- Track WHY a debrid availability probe failed so we can pick a sensible TTL.
--
-- Real-Debrid's May-2026 filter-gate (error_code 35 "Infringing file") rejects
-- releases by filename heuristics. That verdict is durable for a given hash, so
-- re-probing it every 24h wastes API budget and slows every search. Genuinely
-- dead/unseeded torrents can come back, so those keep a short TTL.
ALTER TABLE debrid_availability_cache
    ADD COLUMN reason TEXT;

COMMENT ON COLUMN debrid_availability_cache.reason IS
    'Why the probe failed: blocked (permanent, provider refuses), dead (no seeds/timeout), or NULL for available=true';

-- Search filters on (provider_id, info_hash) with a recency window; the PK
-- covers hash lookups but not the provider+recency scan used to expire rows.
CREATE INDEX idx_debrid_availability_provider_checked
    ON debrid_availability_cache(provider_id, checked_at);
