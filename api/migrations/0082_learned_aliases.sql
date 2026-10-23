CREATE TABLE learned_aliases (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    scope TEXT NOT NULL CHECK (scope IN ('tv_show', 'title', 'sport_want', 'sport_league', 'sport_location')),
    scope_key TEXT NOT NULL,
    scope_label TEXT NOT NULL,
    alias TEXT NOT NULL CHECK (btrim(alias) <> ''),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE UNIQUE INDEX learned_aliases_scope_alias_idx ON learned_aliases (scope, scope_key, lower(alias));
CREATE INDEX learned_aliases_scope_key_idx ON learned_aliases (scope_key);
