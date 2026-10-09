-- Event log for downloads, tracking lifecycle events for debugging
CREATE TABLE download_events (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    download_id UUID NOT NULL REFERENCES downloads(id) ON DELETE CASCADE,
    event_type TEXT NOT NULL,
    message TEXT,
    details JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_download_events_download_id ON download_events(download_id);
