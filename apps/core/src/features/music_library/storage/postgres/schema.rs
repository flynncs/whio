use uuid::Uuid;

pub(crate) type ArtistRow = (Uuid, String, Option<String>, Option<String>);
pub(crate) type AlbumRow = (
    Uuid,
    String,
    Option<Uuid>,
    Option<i16>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);
#[derive(sqlx::FromRow)]
pub(crate) struct AlbumBrowseRow {
    pub(crate) id: Uuid,
    pub(crate) title: String,
    pub(crate) album_artist_id: Option<Uuid>,
    pub(crate) year: Option<i16>,
    pub(crate) release_type: Option<String>,
    pub(crate) label: Option<String>,
    pub(crate) musicbrainz_release_group_id: Option<String>,
    pub(crate) musicbrainz_release_id: Option<String>,
    pub(crate) created_at: chrono::DateTime<chrono::Utc>,
    pub(crate) artist_id: Option<Uuid>,
    pub(crate) artist_name: Option<String>,
    pub(crate) artist_sort_name: Option<String>,
    pub(crate) artist_musicbrainz_id: Option<String>,
    pub(crate) track_count: i64,
    pub(crate) duration_ms: i64,
}
#[derive(sqlx::FromRow)]
pub(crate) struct TrackRecord {
    pub(crate) id: Uuid,
    pub(crate) title: String,
    pub(crate) duration_ms: Option<i64>,
    pub(crate) musicbrainz_recording_id: Option<String>,
    pub(crate) isrc: Option<String>,
    pub(crate) recording_date: Option<chrono::NaiveDate>,
    pub(crate) recording_date_precision: Option<String>,
    pub(crate) original_date: Option<chrono::NaiveDate>,
    pub(crate) original_date_precision: Option<String>,
}

#[derive(sqlx::FromRow)]
pub(crate) struct TrackSearchRow {
    #[sqlx(flatten)]
    pub(crate) track: TrackRecord,
    pub(crate) album_id: Option<Uuid>,
    pub(crate) album_title: Option<String>,
    pub(crate) album_artist_id: Option<Uuid>,
    pub(crate) year: Option<i16>,
    pub(crate) release_type: Option<String>,
    pub(crate) label: Option<String>,
    pub(crate) musicbrainz_release_group_id: Option<String>,
    pub(crate) musicbrainz_release_id: Option<String>,
}

#[derive(sqlx::FromRow)]
pub(crate) struct AlbumEntryRow {
    #[sqlx(flatten)]
    pub(crate) track: TrackRecord,
    pub(crate) disc_number: Option<i16>,
    pub(crate) track_number: Option<i16>,
    pub(crate) position: i32,
    pub(crate) disc_total: Option<i16>,
    pub(crate) track_total: Option<i16>,
}

#[derive(sqlx::FromRow)]
pub(crate) struct TrackPageRow {
    pub(crate) id: Option<Uuid>,
    pub(crate) title: Option<String>,
    pub(crate) duration_ms: Option<i64>,
    pub(crate) musicbrainz_recording_id: Option<String>,
    pub(crate) isrc: Option<String>,
    pub(crate) recording_date: Option<chrono::NaiveDate>,
    pub(crate) recording_date_precision: Option<String>,
    pub(crate) original_date: Option<chrono::NaiveDate>,
    pub(crate) original_date_precision: Option<String>,
    pub(crate) total: i64,
}

pub(crate) const ARTIST_COLUMNS: &str = "id, name, sort_name, musicbrainz_artist_id";
pub(crate) const TRACK_COLUMNS: &str = "t.id, t.title, t.duration_ms, t.musicbrainz_recording_id, \
     t.isrc, t.recording_date, t.recording_date_precision, t.original_date, \
     t.original_date_precision";
pub(crate) const ALBUM_COLUMNS: &str = "id, title, album_artist_id, year, release_type, label, \
     musicbrainz_release_group_id, musicbrainz_release_id";
pub(crate) const ALBUM_SEARCH_COLUMNS: &str = "a.id AS album_id, a.title AS album_title, \
     a.album_artist_id AS album_artist_id, a.year AS year, a.release_type AS release_type, \
     a.label AS label, a.musicbrainz_release_group_id AS musicbrainz_release_group_id, \
     a.musicbrainz_release_id AS musicbrainz_release_id";
pub(crate) const ALBUM_BROWSE_COLUMNS: &str = "a.id, a.title, a.album_artist_id, a.year, a.release_type, \
     a.label, a.musicbrainz_release_group_id, a.musicbrainz_release_id, a.created_at, \
     ar.id AS artist_id, ar.name AS artist_name, ar.sort_name AS artist_sort_name, \
     ar.musicbrainz_artist_id AS artist_musicbrainz_id, \
     COUNT(DISTINCT mt.track_id)::BIGINT AS track_count, \
     COALESCE(SUM(t.duration_ms), 0)::BIGINT AS duration_ms";
pub(crate) const ALBUM_BROWSE_FROM: &str = "FROM albums AS a \
     LEFT JOIN artists AS ar ON ar.id = a.album_artist_id \
     LEFT JOIN album_tracks AS mt ON mt.album_id = a.id \
     LEFT JOIN tracks AS t ON t.id = mt.track_id";
pub(crate) const CREDIT_NORMALIZED: &str = "translate(lower(regexp_replace(normalize(credits.name, NFC), \
     '\\s+', ' ', 'g')), U&'\\2019\\2018\\02BC\\2013\\2014\\2011', \
     U&'\\0027\\0027\\0027\\002D\\002D\\002D')";

// the browse scope. a provider entity is browsable when it is engaged: owned,
// or starred, rated or played by the viewer. each query inlines the clause with
// its own parameter positions, so the numbers here match that query's binds.

// list_albums: $8 is the library provider id list, $9 the viewer.
pub(crate) const ALBUM_ENGAGED: &str = "(EXISTS (
        SELECT 1 FROM album_tracks AS engaged_mt
        JOIN tracks AS t ON t.id = engaged_mt.track_id
        WHERE engaged_mt.album_id = a.id
          AND (EXISTS (
                  SELECT 1 FROM track_sources AS owned
                  WHERE owned.track_id = t.id AND owned.provider_id = ANY($8::TEXT[])
              )
              OR EXISTS (
                  SELECT 1 FROM track_favorites AS fav
                  WHERE fav.track_id = t.id AND fav.user_id = $9
              )
              OR EXISTS (
                  SELECT 1 FROM track_ratings AS rating
                  WHERE rating.track_id = t.id AND rating.user_id = $9
              )
              OR EXISTS (
                  SELECT 1 FROM track_play_stats AS play
                  WHERE play.track_id = t.id AND play.user_id = $9 AND play.play_count > 0
              )
              OR EXISTS (
                  SELECT 1 FROM playlist_entries AS entry
                  JOIN playlists AS list ON list.id = entry.playlist_id
                  WHERE entry.track_id = t.id AND list.user_id = $9
              ))
    )
    OR EXISTS (
        SELECT 1 FROM album_favorites AS fav
        WHERE fav.album_id = a.id AND fav.user_id = $9
    )
    OR EXISTS (
        SELECT 1 FROM album_ratings AS rating
        WHERE rating.album_id = a.id AND rating.user_id = $9
    ))";

// "recently added" means added to the library, so a provider album a search
// merely materialised does not qualify. $8 as above; $9 is unused but keeps the
// bind count identical to ALBUM_ENGAGED.
pub(crate) const ALBUM_OWNED: &str = "($9::UUID IS NOT NULL OR $9::UUID IS NULL) AND EXISTS (
        SELECT 1 FROM album_tracks AS owned_mt
        JOIN track_sources AS owned ON owned.track_id = owned_mt.track_id
        WHERE owned_mt.album_id = a.id
          AND owned.provider_id = ANY($8::TEXT[])
    )";

// list_artists: $3 is the library provider id list, $4 the viewer.
pub(crate) const ARTIST_ENGAGED: &str = "(EXISTS (
        SELECT 1 FROM track_artist_credits AS engaged_credit
        JOIN tracks AS t ON t.id = engaged_credit.track_id
        WHERE engaged_credit.artist_id = ar.id
          AND engaged_credit.role IS NULL
          AND (EXISTS (
                  SELECT 1 FROM track_sources AS owned
                  WHERE owned.track_id = t.id AND owned.provider_id = ANY($3::TEXT[])
              )
              OR EXISTS (
                  SELECT 1 FROM track_favorites AS fav
                  WHERE fav.track_id = t.id AND fav.user_id = $4
              )
              OR EXISTS (
                  SELECT 1 FROM track_ratings AS rating
                  WHERE rating.track_id = t.id AND rating.user_id = $4
              )
              OR EXISTS (
                  SELECT 1 FROM track_play_stats AS play
                  WHERE play.track_id = t.id AND play.user_id = $4 AND play.play_count > 0
              )
              OR EXISTS (
                  SELECT 1 FROM playlist_entries AS entry
                  JOIN playlists AS list ON list.id = entry.playlist_id
                  WHERE entry.track_id = t.id AND list.user_id = $4
              ))
    )
    OR EXISTS (
        SELECT 1 FROM artist_favorites AS fav
        WHERE fav.artist_id = ar.id AND fav.user_id = $4
    )
    OR EXISTS (
        SELECT 1 FROM artist_ratings AS rating
        WHERE rating.artist_id = ar.id AND rating.user_id = $4
    ))";

// count_albums: $1 is the library provider id list, $2 the viewer.
pub(crate) const COUNT_ALBUM_ENGAGED: &str = "(EXISTS (
        SELECT 1 FROM album_tracks AS engaged_mt
        JOIN tracks AS t ON t.id = engaged_mt.track_id
        WHERE engaged_mt.album_id = a.id
          AND (EXISTS (
                  SELECT 1 FROM track_sources AS owned
                  WHERE owned.track_id = t.id AND owned.provider_id = ANY($1::TEXT[])
              )
              OR EXISTS (
                  SELECT 1 FROM track_favorites AS fav
                  WHERE fav.track_id = t.id AND fav.user_id = $2
              )
              OR EXISTS (
                  SELECT 1 FROM track_ratings AS rating
                  WHERE rating.track_id = t.id AND rating.user_id = $2
              )
              OR EXISTS (
                  SELECT 1 FROM track_play_stats AS play
                  WHERE play.track_id = t.id AND play.user_id = $2 AND play.play_count > 0
              )
              OR EXISTS (
                  SELECT 1 FROM playlist_entries AS entry
                  JOIN playlists AS list ON list.id = entry.playlist_id
                  WHERE entry.track_id = t.id AND list.user_id = $2
              ))
    )
    OR EXISTS (
        SELECT 1 FROM album_favorites AS fav
        WHERE fav.album_id = a.id AND fav.user_id = $2
    )
    OR EXISTS (
        SELECT 1 FROM album_ratings AS rating
        WHERE rating.album_id = a.id AND rating.user_id = $2
    ))";

// count_artists: $1 and $2 as above.
pub(crate) const COUNT_ARTIST_ENGAGED: &str = "(EXISTS (
        SELECT 1 FROM track_artist_credits AS engaged_credit
        JOIN tracks AS t ON t.id = engaged_credit.track_id
        WHERE engaged_credit.artist_id = ar.id
          AND engaged_credit.role IS NULL
          AND (EXISTS (
                  SELECT 1 FROM track_sources AS owned
                  WHERE owned.track_id = t.id AND owned.provider_id = ANY($1::TEXT[])
              )
              OR EXISTS (
                  SELECT 1 FROM track_favorites AS fav
                  WHERE fav.track_id = t.id AND fav.user_id = $2
              )
              OR EXISTS (
                  SELECT 1 FROM track_ratings AS rating
                  WHERE rating.track_id = t.id AND rating.user_id = $2
              )
              OR EXISTS (
                  SELECT 1 FROM track_play_stats AS play
                  WHERE play.track_id = t.id AND play.user_id = $2 AND play.play_count > 0
              )
              OR EXISTS (
                  SELECT 1 FROM playlist_entries AS entry
                  JOIN playlists AS list ON list.id = entry.playlist_id
                  WHERE entry.track_id = t.id AND list.user_id = $2
              ))
    )
    OR EXISTS (
        SELECT 1 FROM artist_favorites AS fav
        WHERE fav.artist_id = ar.id AND fav.user_id = $2
    )
    OR EXISTS (
        SELECT 1 FROM artist_ratings AS rating
        WHERE rating.artist_id = ar.id AND rating.user_id = $2
    ))";

pub(crate) const TRACK_ENGAGED: &str = "(EXISTS (
        SELECT 1 FROM track_sources AS owned
        WHERE owned.track_id = t.id AND owned.provider_id = ANY($2::TEXT[])
    )
    OR EXISTS (
        SELECT 1 FROM track_favorites AS fav
        WHERE fav.track_id = t.id AND fav.user_id = $1
    )
    OR EXISTS (
        SELECT 1 FROM track_ratings AS rating
        WHERE rating.track_id = t.id AND rating.user_id = $1
    )
    OR EXISTS (
        SELECT 1 FROM track_play_stats AS play
        WHERE play.track_id = t.id AND play.user_id = $1 AND play.play_count > 0
    )
    OR EXISTS (
        SELECT 1 FROM playlist_entries AS entry
        JOIN playlists AS list ON list.id = entry.playlist_id
        WHERE entry.track_id = t.id AND list.user_id = $1
    ))";

pub(crate) const TRACK_OWNED: &str = "($1::UUID IS NOT NULL OR $1::UUID IS NULL) AND EXISTS (
        SELECT 1 FROM track_sources AS owned
        WHERE owned.track_id = t.id AND owned.provider_id = ANY($2::TEXT[])
    )";

pub(crate) type StateRow = (
    Uuid,
    Option<chrono::DateTime<chrono::Utc>>,
    Option<i16>,
    i64,
    Option<chrono::DateTime<chrono::Utc>>,
);
