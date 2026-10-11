CREATE TABLE acquisition_requests (
    id UUID PRIMARY KEY,
    origin_kind TEXT NOT NULL CHECK (origin_kind IN ('album', 'track', 'release', 'recording')),
    origin_id TEXT NOT NULL CHECK (char_length(origin_id) BETWEEN 1 AND 512),
    origin_key TEXT GENERATED ALWAYS AS (origin_kind || ':' || origin_id) STORED,
    target_kind TEXT CHECK (target_kind IN ('release', 'recording')),
    target_id TEXT CHECK (target_id IS NULL OR char_length(target_id) BETWEEN 1 AND 64),
    artist TEXT CHECK (artist IS NULL OR char_length(artist) BETWEEN 1 AND 500),
    title TEXT CHECK (title IS NULL OR char_length(title) BETWEEN 1 AND 500),
    expected_tracks TEXT NOT NULL DEFAULT '[]',
    candidates TEXT NOT NULL DEFAULT '[]',
    state TEXT NOT NULL CHECK (state IN (
        'pending', 'searching', 'transferring', 'importing', 'completed',
        'already_owned', 'release_not_found', 'release_ambiguous', 'no_candidate',
        'exhausted', 'import_mismatch'
    )),
    reason TEXT,
    attempts INTEGER NOT NULL DEFAULT 0 CHECK (attempts >= 0),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    CHECK ((target_kind IS NULL) = (target_id IS NULL))
);

CREATE INDEX acquisition_requests_origin_idx ON acquisition_requests (origin_key, created_at DESC);

CREATE UNIQUE INDEX acquisition_requests_live_origin_idx ON acquisition_requests (origin_key)
    WHERE state NOT IN (
        'completed', 'already_owned', 'release_not_found', 'release_ambiguous',
        'no_candidate', 'exhausted', 'import_mismatch'
    );

CREATE TABLE acquisition_attempts (
    request_id UUID NOT NULL REFERENCES acquisition_requests (id) ON DELETE CASCADE,
    attempt SMALLINT NOT NULL CHECK (attempt >= 1),
    state TEXT NOT NULL CHECK (state IN ('transferring', 'importing', 'imported', 'failed')),
    reason TEXT,
    files TEXT NOT NULL DEFAULT '[]',
    handles TEXT NOT NULL DEFAULT '[]',
    delivered TEXT NOT NULL DEFAULT '[]',
    imported TEXT NOT NULL DEFAULT '[]',
    finish_intent TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (request_id, attempt)
);

CREATE TABLE acquisition_claims (
    path TEXT PRIMARY KEY,
    request_id UUID NOT NULL,
    attempt SMALLINT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    FOREIGN KEY (request_id, attempt)
        REFERENCES acquisition_attempts (request_id, attempt) ON DELETE CASCADE
);

CREATE INDEX acquisition_claims_attempt_idx ON acquisition_claims (request_id, attempt);
