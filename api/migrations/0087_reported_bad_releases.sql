-- A copy that plays but is broken (audio drifting out of sync, the wrong cut) looks healthy
-- to everything that can be measured. The viewer's word is kept until they withdraw it.
ALTER TABLE stream_release_outcomes ADD COLUMN reported_bad_at TIMESTAMPTZ;
