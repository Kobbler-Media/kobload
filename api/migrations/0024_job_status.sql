-- Job status tracking table for NATS-based job system
-- Local table for UI queries - actual jobs flow through NATS JetStream

CREATE TABLE job_status (
    id UUID PRIMARY KEY,
    job_type TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    payload JSONB,
    result JSONB,
    error TEXT,
    progress SMALLINT DEFAULT 0,
    -- For UI display and linking
    entity_type TEXT,          -- 'download', 'watch_pattern', etc.
    entity_id UUID,            -- Link to related entity
    description TEXT,          -- "Polling download: Some Book"
    -- Timestamps
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    started_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ
);

-- Index for querying jobs by entity
CREATE INDEX idx_job_status_entity ON job_status(entity_type, entity_id)
    WHERE entity_type IS NOT NULL AND entity_id IS NOT NULL;

-- Index for querying active jobs
CREATE INDEX idx_job_status_active ON job_status(status)
    WHERE status IN ('pending', 'running', 'retrying');

-- Index for listing recent jobs
CREATE INDEX idx_job_status_created ON job_status(created_at DESC);
