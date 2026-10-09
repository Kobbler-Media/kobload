-- Add UUID id to each entry in files_json JSONB array
UPDATE downloads
SET files_json = (
    SELECT jsonb_agg(
        jsonb_set(elem, '{id}', to_jsonb(gen_random_uuid()::text))
    )
    FROM jsonb_array_elements(files_json) AS elem
)
WHERE files_json IS NOT NULL AND jsonb_array_length(files_json) > 0;
