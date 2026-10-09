-- Backfill media_type_id on watch_patterns that have media_want but no media_type_id.
-- Without media_type_id, category filtering is completely bypassed for RSS polling,
-- immediate search, and backlog search — allowing wrong-category content to be downloaded.
UPDATE watch_patterns
SET media_type_id = mtc.id
FROM media_type_config mtc
WHERE watch_patterns.media_type_id IS NULL
  AND watch_patterns.media_want IS NOT NULL
  AND mtc.key = CASE watch_patterns.media_want->>'kind'
    WHEN 'book' THEN 'ebooks'
    WHEN 'audiobook' THEN 'audiobooks'
    WHEN 'tv_episode' THEN 'tv'
    WHEN 'tv_season' THEN 'tv'
    WHEN 'sport_event' THEN 'tv'
    WHEN 'sport_season' THEN 'tv'
    WHEN 'movie' THEN 'movies'
    WHEN 'comic' THEN 'comics'
    WHEN 'music' THEN 'music'
    WHEN 'game' THEN 'games'
    WHEN 'software' THEN 'software'
  END;
