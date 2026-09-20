ALTER TABLE track_sources
    DROP CONSTRAINT track_sources_acoustid_fingerprint_check;

ALTER TABLE track_sources
    ADD CONSTRAINT track_sources_acoustid_fingerprint_check CHECK (
        acoustid_fingerprint IS NULL
        OR char_length(acoustid_fingerprint) BETWEEN 1 AND 65536
    );
