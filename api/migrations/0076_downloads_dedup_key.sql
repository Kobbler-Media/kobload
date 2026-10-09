-- Bulletproof submit-time dedup for automated grabs (watch/want). The window-based
-- read-check can race when two watch patterns fire ~1s apart; this index reserves a
-- download_url while a submission is in flight so the second submission is rejected
-- before it reaches the download client. NULL dedup_key (manual/user downloads) is
-- never constrained.
--
-- Predicate covers only in-flight statuses: the index guards against concurrent
-- double-submits, while the recency read-check (find_recent_active_download_by_url,
-- 48h, includes 'completed') handles the later re-grab of an already-imported release.
-- A completed download therefore releases its key rather than locking the URL forever.
ALTER TABLE downloads ADD COLUMN dedup_key TEXT;

CREATE UNIQUE INDEX downloads_dedup_key_active
    ON downloads (dedup_key)
    WHERE dedup_key IS NOT NULL
      AND status IN ('submitting', 'queued', 'downloading', 'pending_retry');
