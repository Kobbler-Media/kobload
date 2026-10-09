-- Gate backlog search for patterns whose content hasn't been released yet.
-- NULL means "search immediately" (default for all existing patterns).
-- RSS matching is unaffected — only backlog search is gated.
ALTER TABLE watch_patterns ADD COLUMN search_not_before TIMESTAMPTZ;
