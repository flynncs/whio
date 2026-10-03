CREATE TABLE IF NOT EXISTS listen_deliveries (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    origin TEXT NOT NULL,
    track_id UUID NOT NULL REFERENCES tracks (id) ON DELETE CASCADE,
    listen_at TIMESTAMPTZ NOT NULL,
    session_key TEXT NOT NULL,
    destination_id UUID NOT NULL,
    provider_id TEXT NOT NULL,
    state TEXT NOT NULL CHECK (state IN ('pending', 'delivered', 'ignored', 'rejected', 'exhausted')),
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    provider_idempotence TEXT NOT NULL CHECK (provider_idempotence = 'timestamp_only'),
    provider_reference TEXT,
    last_error TEXT,
    last_error_class TEXT,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL,
    UNIQUE (user_id, origin, track_id, listen_at, session_key, destination_id)
);

CREATE INDEX IF NOT EXISTS listen_deliveries_user_state_idx
    ON listen_deliveries (user_id, state, updated_at DESC);

CREATE INDEX IF NOT EXISTS listen_deliveries_state_idx
    ON listen_deliveries (state, updated_at);
