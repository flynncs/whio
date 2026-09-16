CREATE TABLE upstream_servers (
    id UUID PRIMARY KEY,
    kind TEXT NOT NULL CHECK (kind = 'navidrome'),
    base_url TEXT NOT NULL CHECK (char_length(base_url) BETWEEN 1 AND 512),
    admin_username TEXT NOT NULL CHECK (char_length(admin_username) BETWEEN 1 AND 200),
    encrypted_admin_password BYTEA NOT NULL,
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE UNIQUE INDEX upstream_servers_base_url_lower_idx ON upstream_servers (lower(base_url));

-- recoverable foreign passwords by design, same envelope as
-- subsonic_app_passwords; rows hold what the connector re-presents upstream
CREATE TABLE upstream_links (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    server_id UUID NOT NULL REFERENCES upstream_servers (id) ON DELETE CASCADE,
    upstream_username TEXT NOT NULL CHECK (char_length(upstream_username) BETWEEN 1 AND 200),
    encrypted_password BYTEA NOT NULL,
    stale_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (user_id, server_id)
);

CREATE INDEX upstream_links_user_idx ON upstream_links (user_id);
