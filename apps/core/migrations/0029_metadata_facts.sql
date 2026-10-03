CREATE TABLE IF NOT EXISTS metadata_facts (
    entity_kind TEXT NOT NULL CHECK (entity_kind IN ('artist', 'album')),
    entity_id UUID NOT NULL,
    field TEXT NOT NULL CHECK (field IN ('identity.name', 'identity.mbid', 'identity.description', 'biography.summary', 'biography.content', 'biography.language', 'image.portrait', 'image.wide', 'page.url', 'tag', 'similar', 'top_tracks', 'metric.listeners', 'metric.plays', 'metric.listen', 'metric.listener')),
    source_provider_id TEXT NOT NULL CHECK (char_length(source_provider_id) BETWEEN 1 AND 64),
    source_external_id TEXT NOT NULL CHECK (char_length(source_external_id) BETWEEN 1 AND 512),
    identity_revision TEXT NOT NULL CHECK (char_length(identity_revision) BETWEEN 1 AND 128),
    language TEXT NOT NULL CHECK (char_length(language) BETWEEN 1 AND 64),
    adapter_version TEXT NOT NULL CHECK (char_length(adapter_version) BETWEEN 1 AND 64),
    value_kind TEXT NOT NULL CHECK (value_kind IN ('text', 'count', 'reference', 'candidate_list')),
    value_text TEXT,
    value_count BIGINT CHECK (value_count IS NULL OR value_count >= 0),
    value_json TEXT,
    unit TEXT CHECK (unit IS NULL OR char_length(unit) BETWEEN 1 AND 64),
    population TEXT CHECK (population IS NULL OR char_length(population) BETWEEN 1 AND 64),
    period TEXT CHECK (period IS NULL OR char_length(period) BETWEEN 1 AND 64),
    as_of TIMESTAMPTZ,
    licence_id TEXT NOT NULL CHECK (char_length(licence_id) BETWEEN 1 AND 128),
    licence_revision INTEGER NOT NULL CHECK (licence_revision >= 0),
    attribution_text TEXT CHECK (attribution_text IS NULL OR char_length(attribution_text) BETWEEN 1 AND 1024),
    attribution_url TEXT CHECK (attribution_url IS NULL OR char_length(attribution_url) BETWEEN 1 AND 2048),
    source_url TEXT CHECK (source_url IS NULL OR char_length(source_url) BETWEEN 1 AND 2048),
    match_basis TEXT NOT NULL CHECK (match_basis IN ('identifier', 'established_source')),
    operation TEXT NOT NULL CHECK (char_length(operation) BETWEEN 1 AND 128),
    terms_basis TEXT CHECK (terms_basis IS NULL OR char_length(terms_basis) BETWEEN 1 AND 512),
    uncertainty BOOLEAN NOT NULL,
    image_role TEXT CHECK (image_role IS NULL OR char_length(image_role) BETWEEN 1 AND 32),
    image_width INTEGER CHECK (image_width IS NULL OR image_width > 0),
    image_height INTEGER CHECK (image_height IS NULL OR image_height > 0),
    image_content_type TEXT CHECK (image_content_type IS NULL OR char_length(image_content_type) BETWEEN 1 AND 128),
    rank INTEGER CHECK (rank IS NULL OR rank >= 0),
    score DOUBLE PRECISION,
    stored_bytes BIGINT NOT NULL CHECK (stored_bytes >= 0),
    fetched_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (entity_kind, entity_id, field, source_provider_id, source_external_id, identity_revision, language, adapter_version),
    CONSTRAINT metadata_facts_value_member CHECK (
        (value_kind IN ('text', 'reference') AND value_text IS NOT NULL AND value_count IS NULL AND value_json IS NULL)
        OR (value_kind = 'count' AND value_count IS NOT NULL AND value_text IS NULL AND value_json IS NULL)
        OR (value_kind = 'candidate_list' AND value_json IS NOT NULL AND value_text IS NULL AND value_count IS NULL)
    ),
    CONSTRAINT metadata_facts_value_json_array CHECK (value_json IS NULL OR jsonb_typeof(value_json::jsonb) = 'array'),
    CONSTRAINT metadata_facts_content_cap CHECK (field <> 'biography.content' OR value_text IS NULL OR octet_length(value_text) <= 65536),
    CONSTRAINT metadata_facts_summary_cap CHECK (field <> 'biography.summary' OR value_text IS NULL OR octet_length(value_text) <= 4096),
    CONSTRAINT metadata_facts_image_value CHECK (field NOT IN ('image.portrait', 'image.wide') OR value_kind = 'reference'),
    CONSTRAINT metadata_facts_metric_value CHECK (field NOT IN ('metric.listeners', 'metric.plays', 'metric.listen', 'metric.listener') OR value_kind = 'count'),
    CONSTRAINT metadata_facts_discovery_value CHECK (field NOT IN ('similar', 'top_tracks') OR value_kind = 'candidate_list'),
    CONSTRAINT metadata_facts_image_members CHECK (
        (field IN ('image.portrait', 'image.wide') AND image_role IS NOT NULL AND image_content_type IS NOT NULL)
        OR (field NOT IN ('image.portrait', 'image.wide') AND image_role IS NULL AND image_width IS NULL AND image_height IS NULL AND image_content_type IS NULL)
    ),
    CONSTRAINT metadata_facts_metric_members CHECK (
        (field IN ('metric.listeners', 'metric.plays', 'metric.listen', 'metric.listener') AND unit IS NOT NULL)
        OR (field NOT IN ('metric.listeners', 'metric.plays', 'metric.listen', 'metric.listener') AND unit IS NULL AND population IS NULL AND period IS NULL AND as_of IS NULL)
    )
);

CREATE INDEX IF NOT EXISTS metadata_facts_subject_idx ON metadata_facts (entity_kind, entity_id, identity_revision, language);

CREATE TABLE IF NOT EXISTS metadata_attempts (
    entity_kind TEXT NOT NULL CHECK (entity_kind IN ('artist', 'album')),
    entity_id UUID NOT NULL,
    field TEXT NOT NULL CHECK (field IN ('identity.name', 'identity.mbid', 'identity.description', 'biography.summary', 'biography.content', 'biography.language', 'image.portrait', 'image.wide', 'page.url', 'tag', 'similar', 'top_tracks', 'metric.listeners', 'metric.plays', 'metric.listen', 'metric.listener')),
    source_provider_id TEXT NOT NULL CHECK (char_length(source_provider_id) BETWEEN 1 AND 64),
    identity_revision TEXT NOT NULL CHECK (char_length(identity_revision) BETWEEN 1 AND 128),
    language TEXT NOT NULL CHECK (char_length(language) BETWEEN 1 AND 64),
    fetched_at TIMESTAMPTZ,
    attempted_at TIMESTAMPTZ NOT NULL,
    last_error TEXT CHECK (last_error IS NULL OR char_length(last_error) BETWEEN 1 AND 512),
    last_error_class TEXT CHECK (last_error_class IS NULL OR char_length(last_error_class) BETWEEN 1 AND 64),
    PRIMARY KEY (entity_kind, entity_id, field, source_provider_id, identity_revision, language),
    CONSTRAINT metadata_attempts_error_complete CHECK ((last_error IS NULL) = (last_error_class IS NULL))
);

CREATE INDEX IF NOT EXISTS metadata_attempts_subject_idx ON metadata_attempts (entity_kind, entity_id, identity_revision, language);
