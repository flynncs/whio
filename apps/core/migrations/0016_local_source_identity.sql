CREATE UNIQUE INDEX IF NOT EXISTS tracks_recording_idx ON tracks (musicbrainz_recording_id)
    WHERE musicbrainz_recording_id IS NOT NULL;

UPDATE track_sources
SET external_id = path
WHERE provider_id = 'local'
  AND path IS NOT NULL
  AND external_id <> path;
