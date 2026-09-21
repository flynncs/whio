CREATE TABLE provider_entity_state (
    provider_id TEXT NOT NULL CHECK (char_length(provider_id) BETWEEN 1 AND 64),
    source_scope TEXT NOT NULL CHECK (char_length(source_scope) BETWEEN 1 AND 512),
    external_id TEXT NOT NULL CHECK (char_length(external_id) BETWEEN 1 AND 512),
    fetched_at TIMESTAMPTZ NOT NULL,
    reported_count INTEGER CHECK (reported_count IS NULL OR reported_count >= 0),
    last_error TEXT CHECK (last_error IS NULL OR char_length(last_error) BETWEEN 1 AND 512),
    PRIMARY KEY (provider_id, source_scope, external_id)
);
