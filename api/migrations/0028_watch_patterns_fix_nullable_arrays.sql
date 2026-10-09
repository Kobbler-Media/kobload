-- Fix nullable array columns that should be NOT NULL with empty array default.
UPDATE watch_patterns SET profile_type_ids = '{}' WHERE profile_type_ids IS NULL;
UPDATE watch_patterns SET profile_ids = '{}' WHERE profile_ids IS NULL;

ALTER TABLE watch_patterns
    ALTER COLUMN profile_type_ids SET NOT NULL,
    ALTER COLUMN profile_ids SET NOT NULL;
