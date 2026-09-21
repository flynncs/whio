CREATE EXTENSION IF NOT EXISTS pg_trgm;

CREATE INDEX IF NOT EXISTS tracks_title_normalized_idx
    ON tracks (title_normalized text_pattern_ops);
CREATE INDEX IF NOT EXISTS tracks_title_trgm_idx
    ON tracks USING GIN (title_normalized gin_trgm_ops);

CREATE INDEX IF NOT EXISTS albums_title_normalized_idx
    ON albums (title_normalized text_pattern_ops);
CREATE INDEX IF NOT EXISTS albums_title_trgm_idx
    ON albums USING GIN (title_normalized gin_trgm_ops);

CREATE INDEX IF NOT EXISTS artists_name_normalized_idx
    ON artists (name_normalized text_pattern_ops);
CREATE INDEX IF NOT EXISTS artists_name_trgm_idx
    ON artists USING GIN (name_normalized gin_trgm_ops);
