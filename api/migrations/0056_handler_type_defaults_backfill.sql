-- Backfill handler_type_defaults for handlers that use Newznab category IDs
-- but were missing defaults. Same pattern as migration 0036 (prowlarr_rss).
--
-- torrentday: map_category() converts TD IDs → Newznab IDs
-- cardigann:  YAML definitions use Newznab category IDs directly
-- mam:        map_category() converts MAM categories → Newznab IDs (7xxx, 3030)
--
-- Without these defaults, resolve_categories_for_source() returns empty
-- for these handler types, and category filtering is completely skipped.

INSERT INTO handler_type_defaults (handler_type, media_type_id, category_ids)
SELECT v.handler_type, htd.media_type_id, htd.category_ids
FROM handler_type_defaults htd
CROSS JOIN (VALUES ('torrentday'), ('cardigann'), ('mam')) AS v(handler_type)
WHERE htd.handler_type = 'newznab'
ON CONFLICT (handler_type, media_type_id) DO NOTHING;
