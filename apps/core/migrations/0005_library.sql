-- playlists are whio-owned because an entry is a reference to a track, and a
-- YouTube Music track has no representation upstream. comment and public are
-- accepted for client compatibility but carry no sharing semantics yet.
CREATE TABLE playlists (
    id UUID PRIMARY KEY,
    user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    name TEXT NOT NULL CHECK (char_length(name) BETWEEN 1 AND 500),
    comment TEXT CHECK (comment IS NULL OR char_length(comment) <= 2000),
    public BOOLEAN NOT NULL DEFAULT FALSE,
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX playlists_user_idx ON playlists (user_id);

-- positional rather than a set: navidrome clients move an entry by index, so
-- position is part of the contract. duplicates are permitted.
CREATE TABLE playlist_entries (
    playlist_id UUID NOT NULL REFERENCES playlists (id) ON DELETE CASCADE,
    position INTEGER NOT NULL CHECK (position >= 0),
    track_id UUID NOT NULL REFERENCES tracks (id) ON DELETE CASCADE,
    PRIMARY KEY (playlist_id, position)
);

CREATE INDEX playlist_entries_track_idx ON playlist_entries (track_id);

CREATE TABLE track_favorites (
    user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    track_id UUID NOT NULL REFERENCES tracks (id) ON DELETE CASCADE,
    starred_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (user_id, track_id)
);

CREATE INDEX track_favorites_track_idx ON track_favorites (track_id);

CREATE TABLE track_ratings (
    user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    track_id UUID NOT NULL REFERENCES tracks (id) ON DELETE CASCADE,
    rating SMALLINT NOT NULL CHECK (rating BETWEEN 1 AND 5),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (user_id, track_id)
);

CREATE INDEX track_ratings_track_idx ON track_ratings (track_id);

-- per-track counters rather than an append-only play log: this is all the
-- client surface needs, and a log can be added later if aggregation is wanted.
CREATE TABLE track_play_stats (
    user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    track_id UUID NOT NULL REFERENCES tracks (id) ON DELETE CASCADE,
    play_count INTEGER NOT NULL DEFAULT 0 CHECK (play_count >= 0),
    last_played_at TIMESTAMPTZ,
    PRIMARY KEY (user_id, track_id)
);

CREATE INDEX track_play_stats_track_idx ON track_play_stats (track_id);

-- one queue per user, keyed by track like every other client-visible reference.
CREATE TABLE play_queues (
    user_id UUID PRIMARY KEY REFERENCES users (id) ON DELETE CASCADE,
    entry_ids UUID[] NOT NULL,
    current INTEGER CHECK (current IS NULL OR current >= 0),
    position_ms INTEGER CHECK (position_ms IS NULL OR position_ms >= 0),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);
