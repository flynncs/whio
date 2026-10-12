ALTER TABLE album_sources
    ADD COLUMN artwork_ref TEXT CHECK (artwork_ref IS NULL OR char_length(artwork_ref) BETWEEN 1 AND 2048);

UPDATE albums
SET release_type = NULLIF(lower(btrim(release_type)), '')
WHERE release_type IS NOT NULL;
