-- Add the "sports" media type. `ensure_default_media_types` (startup.rs) lists
-- it, but only seeds when media_type_config is empty — boxes seeded earlier from
-- the old profile_types table never got it. Without a "sports" row the search UI
-- can't tag a manual sport grab, so sport releases fall back to "misc"/"tv" and
-- parse as movies (losing location/round/session, mis-placing the event).
INSERT INTO media_type_config (key, name)
VALUES ('sports', 'Sports')
ON CONFLICT (key) DO NOTHING;

-- Newznab/Torznab sport categories (5060 = Sport, 5070 = Anime is NOT sport;
-- 5060 is the standard TV/Sport bucket). Map so indexer search can route sports.
INSERT INTO handler_type_defaults (handler_type, media_type_id, category_ids)
SELECT v.handler_type, mtc.id, v.category_ids::TEXT[]
FROM (VALUES
    ('newznab', '{5060,5000}'),
    ('torznab', '{5060,5000}')
) AS v(handler_type, category_ids)
JOIN media_type_config mtc ON mtc.key = 'sports'
ON CONFLICT (handler_type, media_type_id) DO NOTHING;
