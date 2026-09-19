ALTER TABLE tracks
    ADD COLUMN genre TEXT CHECK (genre IS NULL OR char_length(genre) BETWEEN 1 AND 200);

CREATE INDEX tracks_genre_idx ON tracks (lower(genre)) WHERE genre IS NOT NULL;
