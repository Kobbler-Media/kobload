-- A stream cannot repair, so a played file's health no longer depends on PAR2.
ALTER TABLE usenet_releases DROP COLUMN has_par2;

-- An engine that keeps losing a download's job is requeued for free only a few times.
ALTER TABLE downloads ADD COLUMN lost_requeues INTEGER NOT NULL DEFAULT 0;
