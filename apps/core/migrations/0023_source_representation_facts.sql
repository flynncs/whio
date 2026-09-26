ALTER TABLE track_sources
    ADD COLUMN container TEXT CHECK (container IS NULL OR char_length(container) BETWEEN 1 AND 40),
    ADD COLUMN codec TEXT CHECK (codec IS NULL OR char_length(codec) BETWEEN 1 AND 40),
    ADD COLUMN bitrate_kbps INTEGER CHECK (bitrate_kbps IS NULL OR bitrate_kbps > 0),
    ADD COLUMN sample_rate_hz INTEGER CHECK (sample_rate_hz IS NULL OR sample_rate_hz > 0),
    ADD COLUMN bit_depth SMALLINT CHECK (bit_depth IS NULL OR bit_depth > 0),
    ADD COLUMN channels SMALLINT CHECK (channels IS NULL OR channels > 0),
    ADD COLUMN facts_version SMALLINT NOT NULL DEFAULT 0 CHECK (facts_version >= 0);
