CREATE TABLE IF NOT EXISTS destination_accounts (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    provider_id TEXT NOT NULL CHECK (char_length(provider_id) BETWEEN 1 AND 64),
    external_username TEXT CHECK (external_username IS NULL OR char_length(external_username) BETWEEN 1 AND 200),
    encrypted_secret BYTEA NOT NULL,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    last_verified_at TIMESTAMPTZ,
    revoked_at TIMESTAMPTZ,
    UNIQUE (user_id, provider_id)
);

CREATE INDEX IF NOT EXISTS destination_accounts_user_idx ON destination_accounts (user_id);
