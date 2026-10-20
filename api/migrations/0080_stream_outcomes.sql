-- What happened when a client actually played an exact release (hash + file) through a
-- provider. Rows expire (see services/stream/outcomes.rs) so a failure gets retried once
-- whatever caused it may have cleared. A NULL file_id is a single-file release.
CREATE TABLE stream_release_outcomes (
    provider_id UUID NOT NULL REFERENCES debrid_providers(id) ON DELETE CASCADE,
    info_hash TEXT NOT NULL,
    file_id INTEGER,
    successes INTEGER NOT NULL DEFAULT 0,
    failures INTEGER NOT NULL DEFAULT 0,
    last_success_at TIMESTAMPTZ,
    last_failure_at TIMESTAMPTZ,
    last_failure_reason TEXT,
    binge_group TEXT,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    UNIQUE NULLS NOT DISTINCT (provider_id, info_hash, file_id)
);

CREATE INDEX idx_stream_release_outcomes_hash ON stream_release_outcomes(info_hash);

-- Exponentially decayed evidence per release-name trait ("tag:web-dl", "group:sparks").
-- Weights are as of updated_at; readers decay them to now.
CREATE TABLE stream_trait_outcomes (
    provider_id UUID NOT NULL REFERENCES debrid_providers(id) ON DELETE CASCADE,
    trait TEXT NOT NULL,
    success_weight DOUBLE PRECISION NOT NULL DEFAULT 0,
    failure_weight DOUBLE PRECISION NOT NULL DEFAULT 0,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (provider_id, trait)
);
