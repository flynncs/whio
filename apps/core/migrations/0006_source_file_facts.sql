ALTER TABLE track_sources
    ADD COLUMN path TEXT CHECK (path IS NULL OR char_length(path) BETWEEN 1 AND 4096),
    ADD COLUMN size_bytes BIGINT CHECK (size_bytes IS NULL OR size_bytes >= 0),
    ADD COLUMN modified_at TIMESTAMPTZ,
    ADD COLUMN missing_at TIMESTAMPTZ,
    ADD COLUMN content_type TEXT CHECK (content_type IS NULL OR char_length(content_type) BETWEEN 1 AND 200),
    ADD COLUMN suffix TEXT CHECK (suffix IS NULL OR char_length(suffix) BETWEEN 1 AND 20);

CREATE INDEX track_sources_scanned_path_idx
    ON track_sources (provider_id, source_scope, path)
    WHERE path IS NOT NULL;
