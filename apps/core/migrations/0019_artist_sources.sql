CREATE TABLE artist_sources (
    id UUID PRIMARY KEY,
    artist_id UUID NOT NULL REFERENCES artists (id) ON DELETE CASCADE,
    provider_id TEXT NOT NULL CHECK (char_length(provider_id) BETWEEN 1 AND 64),
    source_scope TEXT NOT NULL CHECK (char_length(source_scope) BETWEEN 1 AND 512),
    external_id TEXT NOT NULL CHECK (char_length(external_id) BETWEEN 1 AND 512),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (provider_id, source_scope, external_id)
);

CREATE INDEX artist_sources_artist_idx ON artist_sources (artist_id);
