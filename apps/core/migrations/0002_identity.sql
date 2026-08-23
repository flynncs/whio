CREATE TABLE users (
    id UUID PRIMARY KEY,
    username TEXT NOT NULL CHECK (char_length(username) BETWEEN 1 AND 100),
    display_name TEXT NOT NULL CHECK (char_length(display_name) BETWEEN 1 AND 200),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE UNIQUE INDEX users_username_lower_idx ON users (lower(username));

CREATE TABLE oidc_identities (
    user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    issuer TEXT NOT NULL CHECK (char_length(issuer) BETWEEN 1 AND 512),
    subject TEXT NOT NULL CHECK (char_length(subject) BETWEEN 1 AND 512),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (issuer, subject)
);

-- one-way credentials only; recoverable secret material may never live here
CREATE TABLE api_credentials (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    label TEXT NOT NULL CHECK (char_length(label) BETWEEN 1 AND 200),
    lookup_digest BYTEA NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    last_used_at TIMESTAMPTZ,
    revoked_at TIMESTAMPTZ
);

CREATE UNIQUE INDEX api_credentials_lookup_digest_idx ON api_credentials (lookup_digest);

-- subsonic-compat sandbox; salted-md5 forces recoverable secrets, so these
-- rows are decryptable by design and the whole table is droppable once no
-- client needs token auth anymore
CREATE TABLE subsonic_app_passwords (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    label TEXT NOT NULL CHECK (char_length(label) BETWEEN 1 AND 200),
    encrypted_secret BYTEA NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    last_used_at TIMESTAMPTZ,
    revoked_at TIMESTAMPTZ
);

CREATE INDEX subsonic_app_passwords_user_idx ON subsonic_app_passwords (user_id);
