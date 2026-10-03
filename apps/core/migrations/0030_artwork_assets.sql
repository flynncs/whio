CREATE TABLE IF NOT EXISTS artwork_assets (
    id UUID PRIMARY KEY,
    subject_kind TEXT NOT NULL CHECK (subject_kind IN ('artist')),
    subject_id UUID NOT NULL,
    role TEXT NOT NULL CHECK (role IN ('portrait', 'wide')),
    source_provider_id TEXT NOT NULL CHECK (char_length(source_provider_id) BETWEEN 1 AND 64),
    source_external_id TEXT NOT NULL CHECK (char_length(source_external_id) BETWEEN 1 AND 512),
    revision BIGINT NOT NULL CHECK (revision >= 1),
    variant TEXT NOT NULL CHECK (variant IN ('original', 'px256', 'px768', 'px1600')),
    content_digest TEXT NOT NULL CHECK (char_length(content_digest) = 64),
    content_type TEXT NOT NULL CHECK (char_length(content_type) BETWEEN 1 AND 100),
    width INTEGER NOT NULL CHECK (width > 0),
    height INTEGER NOT NULL CHECK (height > 0),
    byte_size BIGINT NOT NULL CHECK (byte_size > 0),
    file_name TEXT NOT NULL,
    fetched_at TIMESTAMPTZ NOT NULL,
    last_used_at TIMESTAMPTZ NOT NULL,
    UNIQUE (
        subject_kind,
        subject_id,
        role,
        source_provider_id,
        source_external_id,
        revision,
        variant
    )
);

CREATE INDEX IF NOT EXISTS artwork_assets_subject_idx
    ON artwork_assets (subject_kind, subject_id, role);

CREATE INDEX IF NOT EXISTS artwork_assets_last_used_idx
    ON artwork_assets (last_used_at);
