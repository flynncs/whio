CREATE TABLE album_favorites (
    user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    album_id UUID NOT NULL REFERENCES albums (id) ON DELETE CASCADE,
    starred_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (user_id, album_id)
);

CREATE INDEX album_favorites_album_idx ON album_favorites (album_id);

CREATE TABLE artist_favorites (
    user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    artist_id UUID NOT NULL REFERENCES artists (id) ON DELETE CASCADE,
    starred_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (user_id, artist_id)
);

CREATE INDEX artist_favorites_artist_idx ON artist_favorites (artist_id);

CREATE TABLE album_ratings (
    user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    album_id UUID NOT NULL REFERENCES albums (id) ON DELETE CASCADE,
    rating SMALLINT NOT NULL CHECK (rating BETWEEN 1 AND 5),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (user_id, album_id)
);

CREATE INDEX album_ratings_album_idx ON album_ratings (album_id);

CREATE TABLE artist_ratings (
    user_id UUID NOT NULL REFERENCES users (id) ON DELETE CASCADE,
    artist_id UUID NOT NULL REFERENCES artists (id) ON DELETE CASCADE,
    rating SMALLINT NOT NULL CHECK (rating BETWEEN 1 AND 5),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (user_id, artist_id)
);

CREATE INDEX artist_ratings_artist_idx ON artist_ratings (artist_id);
