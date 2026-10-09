-- Reject low-quality sources (CAM, TS, DVDScr, etc.) from all quality profiles.
-- These are unwatchable for debrid streaming and waste debrid API calls.
UPDATE quality_profiles
SET rejected_sources = '{cam,ts,telecine,dvdscr,screener,telesync,hdcam}'
WHERE rejected_sources = '{cam,ts,telecine}'
   OR rejected_sources IS NULL
   OR rejected_sources = '{}';
