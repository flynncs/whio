ALTER TABLE albums ADD COLUMN provider_source_key TEXT CHECK (
    provider_source_key IS NULL OR char_length(provider_source_key) BETWEEN 1 AND 600
);

UPDATE albums AS album
SET provider_source_key = chosen.provider_source_key
FROM (
    SELECT DISTINCT ON (album_id)
        album_id,
        provider_id || ':' || external_id AS provider_source_key
    FROM album_sources
    ORDER BY album_id, created_at, provider_id, external_id, id
) AS chosen
WHERE album.id = chosen.album_id
  AND NOT EXISTS (
      SELECT 1 FROM album_sources AS library_source
      WHERE library_source.album_id = album.id
        AND library_source.provider_id = 'local'
  );

DROP INDEX albums_text_idx;

CREATE UNIQUE INDEX albums_text_idx ON albums (album_artist_id, title_normalized, provider_source_key)
    NULLS NOT DISTINCT WHERE musicbrainz_release_group_id IS NULL;
