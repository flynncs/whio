CREATE TABLE IF NOT EXISTS listens (
    user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    origin TEXT NOT NULL CHECK (origin IN ('subsonic_scrobble', 'native', 'report_playback')),
    track_id UUID NOT NULL REFERENCES tracks (id) ON DELETE CASCADE,
    listen_at TIMESTAMPTZ NOT NULL,
    session_id TEXT CHECK (session_id IS NULL OR char_length(session_id) BETWEEN 1 AND 128),
    start_position_ms BIGINT CHECK (start_position_ms IS NULL OR start_position_ms >= 0),
    listened_ms BIGINT CHECK (listened_ms IS NULL OR listened_ms >= 0),
    qualified BOOLEAN,
    session_key TEXT GENERATED ALWAYS AS (COALESCE(session_id, '')) STORED,
    PRIMARY KEY (user_id, origin, track_id, listen_at, session_key),
    CONSTRAINT listens_extent_complete CHECK ((start_position_ms IS NULL) = (listened_ms IS NULL)),
    CONSTRAINT listens_qualification_complete CHECK ((listened_ms IS NULL) = (qualified IS NULL))
);

CREATE INDEX IF NOT EXISTS listens_user_track_time_idx ON listens (user_id, track_id, listen_at);

CREATE INDEX IF NOT EXISTS listens_user_time_idx ON listens (user_id, listen_at);

CREATE INDEX IF NOT EXISTS listens_track_idx ON listens (track_id);

CREATE TABLE IF NOT EXISTS listen_marks (
    user_id UUID PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
    started_at TIMESTAMPTZ NOT NULL
);

INSERT INTO listen_marks (user_id, started_at)
SELECT id, CURRENT_TIMESTAMP FROM users
ON CONFLICT (user_id) DO NOTHING;
