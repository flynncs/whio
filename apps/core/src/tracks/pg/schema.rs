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
pub(crate) type TrackSearchRow = (
    Uuid,
    String,
    Option<i64>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<Uuid>,
    Option<String>,
    Option<Uuid>,
    Option<i16>,
    Option<String>,
    Option<String>,
    Option<String>,
    Option<String>,
);

pub(crate) const ARTIST_COLUMNS: &str = "id, name, sort_name, musicbrainz_artist_id";
pub(crate) const ALBUM_COLUMNS: &str = "id, title, album_artist_id, year, release_type, label, \
     musicbrainz_release_group_id, musicbrainz_release_id";
pub(crate) const ALBUM_SEARCH_COLUMNS: &str = "a.id, a.title, a.album_artist_id, a.year, a.release_type, \
     a.label, a.musicbrainz_release_group_id, a.musicbrainz_release_id";
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

pub(crate) type StateRow = (
    Uuid,
    Option<chrono::DateTime<chrono::Utc>>,
    Option<i16>,
    i64,
    Option<chrono::DateTime<chrono::Utc>>,
);
