-- Add handler_type_defaults for prowlarr_rss.
-- Copies the same categories as newznab since prowlarr_rss uses newznab categories.
-- Uses ON CONFLICT DO NOTHING to not overwrite any user customizations.

INSERT INTO handler_type_defaults (handler_type, media_type_id, category_ids)
SELECT 'prowlarr_rss', media_type_id, category_ids
FROM handler_type_defaults
WHERE handler_type = 'newznab'
ON CONFLICT (handler_type, media_type_id) DO NOTHING;
