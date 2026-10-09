-- Allow deleting downloads that have associated watch_matches.
-- Previously the FK defaulted to RESTRICT, blocking deletion.
ALTER TABLE watch_matches
    DROP CONSTRAINT IF EXISTS watch_matches_download_id_fkey,
    ADD CONSTRAINT watch_matches_download_id_fkey
        FOREIGN KEY (download_id)
        REFERENCES downloads(id)
        ON DELETE SET NULL;
