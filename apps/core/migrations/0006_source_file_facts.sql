-- per-source file facts rather than a local-only table: a remote source carries
-- the same facts and simply does not learn them yet. size and modification time
-- also let a rescan skip files that have not changed.
ALTER TABLE track_sources
    ADD COLUMN size_bytes BIGINT CHECK (size_bytes IS NULL OR size_bytes >= 0),
    ADD COLUMN modified_at TIMESTAMPTZ,
    ADD COLUMN content_type TEXT CHECK (content_type IS NULL OR char_length(content_type) BETWEEN 1 AND 200),
    ADD COLUMN suffix TEXT CHECK (suffix IS NULL OR char_length(suffix) BETWEEN 1 AND 20);
