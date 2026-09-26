CREATE TABLE track_genres (
    track_id UUID NOT NULL REFERENCES tracks (id) ON DELETE CASCADE,
    position INTEGER NOT NULL CHECK (position >= 0),
    name TEXT NOT NULL CHECK (char_length(name) BETWEEN 1 AND 200),
    name_normalized TEXT GENERATED ALWAYS AS (
        lower(regexp_replace(normalize(name, NFC), '\s+', ' ', 'g'))
    ) STORED,
    PRIMARY KEY (track_id, position),
    UNIQUE (track_id, name_normalized)
);

CREATE INDEX track_genres_name_idx ON track_genres (name_normalized);

ALTER TABLE track_artist_credits
    ADD COLUMN role TEXT CHECK (role IS NULL OR char_length(role) BETWEEN 1 AND 40);

ALTER TABLE tracks
    ADD COLUMN recording_date DATE,
    ADD COLUMN recording_date_precision TEXT CHECK (recording_date_precision IN ('year', 'month', 'day')),
    ADD COLUMN original_date DATE,
    ADD COLUMN original_date_precision TEXT CHECK (original_date_precision IN ('year', 'month', 'day'));

ALTER TABLE albums
    ADD COLUMN release_date DATE,
    ADD COLUMN release_date_precision TEXT CHECK (release_date_precision IN ('year', 'month', 'day')),
    ADD COLUMN release_country TEXT CHECK (release_country IS NULL OR char_length(release_country) BETWEEN 1 AND 120),
    ADD COLUMN compilation BOOLEAN;

ALTER TABLE album_tracks
    ADD COLUMN disc_total SMALLINT CHECK (disc_total IS NULL OR disc_total >= 1),
    ADD COLUMN track_total SMALLINT CHECK (track_total IS NULL OR track_total >= 1);
