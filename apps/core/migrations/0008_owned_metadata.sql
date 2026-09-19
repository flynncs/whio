CREATE TABLE artists (
    id UUID PRIMARY KEY,
    name TEXT NOT NULL CHECK (char_length(name) BETWEEN 1 AND 500),
    name_normalized TEXT GENERATED ALWAYS AS (
        lower(regexp_replace(normalize(name, NFC), '\s+', ' ', 'g'))
    ) STORED,
    sort_name TEXT CHECK (sort_name IS NULL OR char_length(sort_name) BETWEEN 1 AND 500),
    musicbrainz_artist_id TEXT CHECK (
        musicbrainz_artist_id IS NULL OR char_length(musicbrainz_artist_id) BETWEEN 1 AND 64
    ),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE UNIQUE INDEX artists_musicbrainz_idx ON artists (musicbrainz_artist_id)
    WHERE musicbrainz_artist_id IS NOT NULL;

CREATE UNIQUE INDEX artists_name_idx ON artists (name_normalized)
    WHERE musicbrainz_artist_id IS NULL;

CREATE TABLE albums (
    id UUID PRIMARY KEY,
    title TEXT NOT NULL CHECK (char_length(title) BETWEEN 1 AND 500),
    title_normalized TEXT GENERATED ALWAYS AS (
        lower(regexp_replace(normalize(title, NFC), '\s+', ' ', 'g'))
    ) STORED,
    album_artist_id UUID REFERENCES artists (id) ON DELETE SET NULL,
    year SMALLINT CHECK (year IS NULL OR year BETWEEN 1000 AND 3000),
    release_type TEXT CHECK (release_type IS NULL OR char_length(release_type) BETWEEN 1 AND 64),
    label TEXT CHECK (label IS NULL OR char_length(label) BETWEEN 1 AND 500),
    musicbrainz_release_group_id TEXT CHECK (
        musicbrainz_release_group_id IS NULL
        OR char_length(musicbrainz_release_group_id) BETWEEN 1 AND 64
    ),
    musicbrainz_release_id TEXT CHECK (
        musicbrainz_release_id IS NULL OR char_length(musicbrainz_release_id) BETWEEN 1 AND 64
    ),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE UNIQUE INDEX albums_release_group_idx ON albums (musicbrainz_release_group_id)
    WHERE musicbrainz_release_group_id IS NOT NULL;

CREATE UNIQUE INDEX albums_release_idx ON albums (musicbrainz_release_id)
    WHERE musicbrainz_release_id IS NOT NULL;

CREATE UNIQUE INDEX albums_text_idx ON albums (album_artist_id, title_normalized)
    NULLS NOT DISTINCT WHERE musicbrainz_release_group_id IS NULL;

CREATE INDEX albums_text_lookup_idx ON albums (album_artist_id, title_normalized);

ALTER TABLE tracks
    ADD COLUMN musicbrainz_recording_id TEXT CHECK (
        musicbrainz_recording_id IS NULL OR char_length(musicbrainz_recording_id) BETWEEN 1 AND 64
    ),
    ADD COLUMN isrc TEXT CHECK (isrc IS NULL OR char_length(isrc) BETWEEN 1 AND 32);

ALTER TABLE track_sources
    ADD COLUMN acoustid_fingerprint TEXT CHECK (
        acoustid_fingerprint IS NULL OR char_length(acoustid_fingerprint) BETWEEN 1 AND 512
    );

ALTER TABLE track_artist_credits
    ADD COLUMN artist_id UUID REFERENCES artists (id) ON DELETE SET NULL;

CREATE TABLE album_tracks (
    album_id UUID NOT NULL REFERENCES albums (id) ON DELETE CASCADE,
    track_id UUID NOT NULL REFERENCES tracks (id) ON DELETE CASCADE,
    disc_number SMALLINT CHECK (disc_number IS NULL OR disc_number >= 1),
    track_number SMALLINT CHECK (track_number IS NULL OR track_number >= 1),
    position INTEGER NOT NULL CHECK (position >= 0),
    PRIMARY KEY (album_id, track_id)
);

CREATE INDEX album_tracks_track_idx ON album_tracks (track_id);

CREATE TABLE album_sources (
    id UUID PRIMARY KEY,
    album_id UUID NOT NULL REFERENCES albums (id) ON DELETE CASCADE,
    provider_id TEXT NOT NULL CHECK (char_length(provider_id) BETWEEN 1 AND 64),
    source_scope TEXT NOT NULL CHECK (char_length(source_scope) BETWEEN 1 AND 512),
    external_id TEXT NOT NULL CHECK (char_length(external_id) BETWEEN 1 AND 512),
    created_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE (provider_id, source_scope, external_id)
);

CREATE INDEX album_sources_album_idx ON album_sources (album_id);
