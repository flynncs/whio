CREATE TABLE tracks (
    id UUID PRIMARY KEY,
    title TEXT NOT NULL CHECK (char_length(title) BETWEEN 1 AND 500),
    duration_ms BIGINT CHECK (duration_ms IS NULL OR duration_ms >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE track_artist_credits (
    track_id UUID NOT NULL REFERENCES tracks (id) ON DELETE CASCADE,
    position INTEGER NOT NULL CHECK (position >= 0),
    name TEXT NOT NULL CHECK (char_length(name) BETWEEN 1 AND 500),
    PRIMARY KEY (track_id, position)
);

CREATE TABLE track_sources (
    id UUID PRIMARY KEY,
    track_id UUID NOT NULL REFERENCES tracks (id) ON DELETE CASCADE,
    provider_id TEXT NOT NULL CHECK (char_length(provider_id) BETWEEN 1 AND 64),
    source_scope TEXT NOT NULL CHECK (char_length(source_scope) BETWEEN 1 AND 512),
    external_id TEXT NOT NULL CHECK (char_length(external_id) BETWEEN 1 AND 512),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (provider_id, source_scope, external_id)
);

CREATE INDEX track_sources_track_id_idx ON track_sources (track_id);
