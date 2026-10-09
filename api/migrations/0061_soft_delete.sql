-- Soft-delete for downloads and watch matches.
--
-- When users clear completed downloads or match history from the UI,
-- rows are hidden instead of deleted. The watch system still sees hidden
-- rows for dedup, upgrade decisions, and import confirmation.

ALTER TABLE downloads ADD COLUMN hidden BOOLEAN NOT NULL DEFAULT FALSE;
ALTER TABLE watch_matches ADD COLUMN hidden BOOLEAN NOT NULL DEFAULT FALSE;
