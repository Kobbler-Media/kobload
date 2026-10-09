-- Hash session tokens and add expiry for security hardening.
-- Truncates all existing sessions (users must re-login).
TRUNCATE auth_tokens;

-- Rename plaintext token column to token_hash
ALTER TABLE auth_tokens RENAME COLUMN token TO token_hash;

-- Add expiry column (30-day default TTL)
ALTER TABLE auth_tokens ADD COLUMN expires_at TIMESTAMPTZ NOT NULL DEFAULT NOW() + INTERVAL '30 days';
