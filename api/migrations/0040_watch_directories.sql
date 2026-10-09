-- Watch directories: monitor filesystem paths for new media files.
-- Discovered files are sent via callback to an external service (e.g. Kobbler).

CREATE TABLE watch_directories (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    path TEXT NOT NULL UNIQUE,
    name TEXT NOT NULL,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    recursive BOOLEAN NOT NULL DEFAULT TRUE,
    media_type TEXT NOT NULL DEFAULT 'book',
    callback_url TEXT NOT NULL,
    auth_token TEXT,
    scan_status TEXT NOT NULL DEFAULT 'idle',
    scan_error TEXT,
    error_count INTEGER NOT NULL DEFAULT 0,
    next_retry_at TIMESTAMPTZ,
    last_scan_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE TABLE watch_directory_files (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    watch_directory_id UUID NOT NULL REFERENCES watch_directories(id) ON DELETE CASCADE,
    file_path TEXT NOT NULL,
    file_name TEXT NOT NULL,
    file_size BIGINT NOT NULL,
    file_hash TEXT,
    file_mtime TIMESTAMPTZ,
    interpretation JSONB,
    status TEXT NOT NULL DEFAULT 'discovered',
    download_id UUID REFERENCES downloads(id) ON DELETE SET NULL,
    callback_sent_at TIMESTAMPTZ,
    callback_error TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE(watch_directory_id, file_path)
);

CREATE INDEX idx_watch_directory_files_dir ON watch_directory_files(watch_directory_id);
CREATE INDEX idx_watch_directory_files_status ON watch_directory_files(status);
