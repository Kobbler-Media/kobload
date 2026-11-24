-- Deleting a download client keeps the history of what it downloaded.
ALTER TABLE downloads
    DROP CONSTRAINT IF EXISTS downloads_download_client_id_fkey,
    ADD CONSTRAINT downloads_download_client_id_fkey
        FOREIGN KEY (download_client_id)
        REFERENCES download_clients(id)
        ON DELETE SET NULL;
