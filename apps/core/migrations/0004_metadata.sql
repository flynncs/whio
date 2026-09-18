ALTER TABLE tracks
    ADD COLUMN album TEXT CHECK (album IS NULL OR char_length(album) BETWEEN 1 AND 500),
    ADD COLUMN album_source TEXT CHECK (album_source IS NULL OR char_length(album_source) BETWEEN 1 AND 64);

ALTER TABLE track_sources
    ADD COLUMN artwork_kind TEXT CHECK (artwork_kind IS NULL OR artwork_kind IN ('upstream', 'url')),
    ADD COLUMN artwork_ref TEXT CHECK (artwork_ref IS NULL OR char_length(artwork_ref) BETWEEN 1 AND 2048);
