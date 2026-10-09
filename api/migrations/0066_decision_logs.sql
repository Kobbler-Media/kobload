-- Decision logging: captures the full search/feed pipeline decision trace
-- for debugging and analysis. Three-level hierarchy: session → provider_calls → releases.

CREATE TABLE decision_log_sessions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    trigger_type TEXT NOT NULL, -- 'backlog_search', 'immediate_search', 'want', 'feed_sync', 'test', 'manual'
    watch_pattern_id UUID REFERENCES watch_patterns(id) ON DELETE SET NULL,
    indexer_source_id UUID REFERENCES indexer_sources(id) ON DELETE SET NULL,
    is_test BOOLEAN NOT NULL DEFAULT FALSE,
    outcome TEXT, -- 'downloaded', 'held', 'no_match', 'rejected', 'error', 'dry_run'
    download_id UUID REFERENCES downloads(id) ON DELETE SET NULL,
    releases_evaluated INTEGER NOT NULL DEFAULT 0,
    search_queries JSONB,
    error_message TEXT,
    started_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    finished_at TIMESTAMPTZ,
    duration_ms INTEGER,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_dls_trigger_type ON decision_log_sessions(trigger_type);
CREATE INDEX idx_dls_pattern ON decision_log_sessions(watch_pattern_id) WHERE watch_pattern_id IS NOT NULL;
CREATE INDEX idx_dls_created_at ON decision_log_sessions(created_at DESC);
CREATE INDEX idx_dls_is_test ON decision_log_sessions(is_test) WHERE is_test = TRUE;

CREATE TABLE decision_log_provider_calls (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    session_id UUID NOT NULL REFERENCES decision_log_sessions(id) ON DELETE CASCADE,
    indexer_source_id UUID REFERENCES indexer_sources(id) ON DELETE SET NULL,
    source_name TEXT NOT NULL,
    handler_type TEXT NOT NULL,
    search_type TEXT, -- 'structured', 'text', 'rss'
    query TEXT,
    categories JSONB,
    external_ids JSONB,
    results_count INTEGER NOT NULL DEFAULT 0,
    results_after_dedup INTEGER NOT NULL DEFAULT 0,
    status TEXT NOT NULL, -- 'success', 'error', 'rate_limited', 'skipped'
    error_message TEXT,
    duration_ms INTEGER,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_dlpc_session ON decision_log_provider_calls(session_id);

CREATE TABLE decision_log_releases (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    session_id UUID NOT NULL REFERENCES decision_log_sessions(id) ON DELETE CASCADE,
    provider_call_id UUID REFERENCES decision_log_provider_calls(id) ON DELETE SET NULL,
    release_title TEXT NOT NULL,
    release_guid TEXT,
    release_size BIGINT,
    protocol TEXT,
    indexer_name TEXT,
    match_score DOUBLE PRECISION,
    matched_pattern TEXT,
    match_mode TEXT, -- 'fuzzy', 'any', 'all', 'target'
    quality_score DOUBLE PRECISION,
    quality_score_breakdown JSONB,
    total_score DOUBLE PRECISION,
    parsed_quality JSONB,
    structural_result JSONB,
    decision TEXT NOT NULL, -- 'best', 'candidate', 'rejected_quality', 'rejected_match', 'rejected_structural', 'rejected_duplicate', 'rejected_trash', 'rejected_category', 'rejected_upgrade', 'rejected_excluded'
    decision_reason TEXT,
    episode_key TEXT,
    is_best BOOLEAN NOT NULL DEFAULT FALSE,
    release_data JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_dlr_session ON decision_log_releases(session_id);
CREATE INDEX idx_dlr_best ON decision_log_releases(session_id) WHERE is_best = TRUE;
