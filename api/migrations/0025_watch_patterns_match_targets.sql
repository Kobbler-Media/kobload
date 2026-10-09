-- Add match_targets to watch_patterns for improved scoring
-- match_targets is a JSONB array of {id, target, min_score} objects
-- Results are scored against each target; the target with the highest score determines matched_target_id
ALTER TABLE watch_patterns ADD COLUMN match_targets JSONB DEFAULT '[]';
