-- Whether the listing's NZB carries PAR2 recovery files, read when kobload fetches it. NULL
-- until then: an NZB the engine already held was never seen here.
ALTER TABLE usenet_releases ADD COLUMN has_par2 BOOLEAN;
