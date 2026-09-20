DROP INDEX albums_text_idx;
DROP INDEX albums_text_lookup_idx;
DROP INDEX artists_name_idx;

ALTER TABLE tracks DROP COLUMN title_normalized;
ALTER TABLE albums DROP COLUMN title_normalized;
ALTER TABLE artists DROP COLUMN name_normalized;

ALTER TABLE tracks
    ADD COLUMN title_normalized TEXT GENERATED ALWAYS AS (
        translate(
            lower(regexp_replace(normalize(title, NFC), '\s+', ' ', 'g')),
            U&'\2019\2018\02BC\2013\2014\2011',
            U&'\0027\0027\0027\002D\002D\002D'
        )
    ) STORED;

ALTER TABLE albums
    ADD COLUMN title_normalized TEXT GENERATED ALWAYS AS (
        translate(
            lower(regexp_replace(normalize(title, NFC), '\s+', ' ', 'g')),
            U&'\2019\2018\02BC\2013\2014\2011',
            U&'\0027\0027\0027\002D\002D\002D'
        )
    ) STORED;

ALTER TABLE artists
    ADD COLUMN name_normalized TEXT GENERATED ALWAYS AS (
        translate(
            lower(regexp_replace(normalize(name, NFC), '\s+', ' ', 'g')),
            U&'\2019\2018\02BC\2013\2014\2011',
            U&'\0027\0027\0027\002D\002D\002D'
        )
    ) STORED;

CREATE UNIQUE INDEX albums_text_idx ON albums (album_artist_id, title_normalized)
    NULLS NOT DISTINCT WHERE musicbrainz_release_group_id IS NULL;

CREATE INDEX albums_text_lookup_idx ON albums (album_artist_id, title_normalized);

CREATE UNIQUE INDEX artists_name_idx ON artists (name_normalized)
    WHERE musicbrainz_artist_id IS NULL;
