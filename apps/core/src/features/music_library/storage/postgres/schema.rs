use uuid::Uuid;

use crate::features::music_library::domain::AlbumListScope;

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

pub(crate) fn folded(value: &str) -> String {
    format!(
        "translate(lower(regexp_replace(normalize({value}, NFC), '\\s+', ' ', 'g')), \
         U&'\\2019\\2018\\02BC\\2013\\2014\\2011', U&'\\0027\\0027\\0027\\002D\\002D\\002D')"
    )
}

pub(crate) fn search_folded(value: &str) -> String {
    let decomposed = format!("normalize({}, NFD)", folded(value));
    format!("normalize(regexp_replace({decomposed}, U&'[\\0300-\\036F]', '', 'g'), NFC)")
}

pub(crate) fn primary_album_order(album: &str, library_bind: u8) -> String {
    format!(
        "NOT EXISTS (SELECT 1 FROM album_sources AS library_album \
         WHERE library_album.album_id = {album}.id \
           AND library_album.provider_id = ANY(${library_bind}::TEXT[])), \
         NOT EXISTS (SELECT 1 FROM album_tracks AS library_member \
         JOIN track_sources AS library_member_source \
           ON library_member_source.track_id = library_member.track_id \
         WHERE library_member.album_id = {album}.id \
           AND library_member_source.provider_id = ANY(${library_bind}::TEXT[])), \
         {album}.id"
    )
}

// the browse scope. a provider entity is browsable when it is engaged: owned,
// or starred, rated or played by the viewer. each query inlines the clause with
// its own parameter positions, so the numbers here match that query's binds.

pub(crate) fn album_scope(scope: AlbumListScope, user_bind: u8, libraries_bind: u8) -> String {
    match scope {
        AlbumListScope::Engaged => format!(
            "(EXISTS (
        SELECT 1 FROM album_tracks AS engaged_mt
        JOIN tracks AS t ON t.id = engaged_mt.track_id
        WHERE engaged_mt.album_id = a.id
          AND (EXISTS (
                  SELECT 1 FROM track_sources AS owned
                  WHERE owned.track_id = t.id AND owned.provider_id = ANY(${libraries_bind}::TEXT[])
              )
              OR EXISTS (
                  SELECT 1 FROM track_favorites AS fav
                  WHERE fav.track_id = t.id AND fav.user_id = ${user_bind}
              )
              OR EXISTS (
                  SELECT 1 FROM track_ratings AS rating
                  WHERE rating.track_id = t.id AND rating.user_id = ${user_bind}
              )
              OR EXISTS (
                  SELECT 1 FROM track_play_stats AS play
                  WHERE play.track_id = t.id AND play.user_id = ${user_bind} AND play.play_count > 0
              )
              OR EXISTS (
                  SELECT 1 FROM playlist_entries AS entry
                  JOIN playlists AS list ON list.id = entry.playlist_id
                  WHERE entry.track_id = t.id AND list.user_id = ${user_bind}
              )
              OR EXISTS (
                  SELECT 1 FROM restore_queue_entries AS queued
                  WHERE queued.track_id = t.id AND queued.queue_user_id = ${user_bind}
              ))
    )
    OR EXISTS (
        SELECT 1 FROM album_favorites AS fav
        WHERE fav.album_id = a.id AND fav.user_id = ${user_bind}
    )
    OR EXISTS (
        SELECT 1 FROM album_ratings AS rating
        WHERE rating.album_id = a.id AND rating.user_id = ${user_bind}
    ))"
        ),
        AlbumListScope::EngagedOrDiscovered => format!(
            "({} OR EXISTS (SELECT 1 FROM album_sources AS discovered_album \
             WHERE discovered_album.album_id = a.id \
               AND discovered_album.provider_id <> ALL(${libraries_bind}::TEXT[])))",
            album_scope(AlbumListScope::Engaged, user_bind, libraries_bind),
        ),
        AlbumListScope::Owned => format!(
            "(${user_bind}::UUID IS NOT NULL OR ${user_bind}::UUID IS NULL) AND EXISTS (
        SELECT 1 FROM album_tracks AS owned_mt
        JOIN track_sources AS owned ON owned.track_id = owned_mt.track_id
        WHERE owned_mt.album_id = a.id
          AND owned.provider_id = ANY(${libraries_bind}::TEXT[])
    )"
        ),
    }
}

pub(crate) fn track_engaged(user_bind: u8, libraries_bind: u8) -> String {
    format!(
        "(EXISTS (
        SELECT 1 FROM track_sources AS engaged_source
        WHERE engaged_source.track_id = t.id
          AND engaged_source.provider_id = ANY(${libraries_bind}::TEXT[])
    )
    OR EXISTS (
        SELECT 1 FROM track_favorites AS engaged_favorite
        WHERE engaged_favorite.track_id = t.id AND engaged_favorite.user_id = ${user_bind}
    )
    OR EXISTS (
        SELECT 1 FROM track_ratings AS engaged_rating
        WHERE engaged_rating.track_id = t.id AND engaged_rating.user_id = ${user_bind}
    )
    OR EXISTS (
        SELECT 1 FROM track_play_stats AS engaged_play
        WHERE engaged_play.track_id = t.id AND engaged_play.user_id = ${user_bind}
          AND engaged_play.play_count > 0
    )
    OR EXISTS (
        SELECT 1 FROM playlist_entries AS engaged_entry
        JOIN playlists AS engaged_list ON engaged_list.id = engaged_entry.playlist_id
        WHERE engaged_entry.track_id = t.id AND engaged_list.user_id = ${user_bind}
    )
    OR EXISTS (
        SELECT 1 FROM restore_queue_entries AS engaged_queue
        WHERE engaged_queue.track_id = t.id AND engaged_queue.queue_user_id = ${user_bind}
    ))"
    )
}

pub(crate) fn track_owned(user_bind: u8, libraries_bind: u8) -> String {
    format!(
        "(${user_bind}::UUID IS NOT NULL OR ${user_bind}::UUID IS NULL) AND EXISTS (
        SELECT 1 FROM track_sources AS owned
        WHERE owned.track_id = t.id AND owned.provider_id = ANY(${libraries_bind}::TEXT[])
    )"
    )
}

pub(crate) type StateRow = (
    Uuid,
    Option<chrono::DateTime<chrono::Utc>>,
    Option<i16>,
    i64,
    Option<chrono::DateTime<chrono::Utc>>,
);

pub(crate) fn artist_owned_scope(user_bind: u8, libraries_bind: u8) -> String {
    let owned_album = album_scope(AlbumListScope::Owned, user_bind, libraries_bind);

    format!(
        "(EXISTS (
        SELECT 1 FROM track_artist_credits AS owned_credit
        JOIN track_sources AS owned ON owned.track_id = owned_credit.track_id
        WHERE owned_credit.artist_id = ar.id
          AND owned.provider_id = ANY(${libraries_bind}::TEXT[])
    )
    OR EXISTS (
        SELECT 1 FROM albums AS a
        WHERE a.album_artist_id = ar.id AND {owned_album}
    ))"
    )
}

pub(crate) fn artist_credited_engagement(user_bind: u8, libraries_bind: u8) -> String {
    let engaged_track = track_engaged(user_bind, libraries_bind);

    format!(
        "(EXISTS (
        SELECT 1 FROM track_artist_credits AS credited
        JOIN tracks AS t ON t.id = credited.track_id
        WHERE credited.artist_id = ar.id
          AND {engaged_track}
    )
    OR EXISTS (
        SELECT 1 FROM artist_favorites AS credited_favorite
        WHERE credited_favorite.artist_id = ar.id
          AND credited_favorite.user_id = ${user_bind}
    )
    OR EXISTS (
        SELECT 1 FROM artist_ratings AS credited_rating
        WHERE credited_rating.artist_id = ar.id
          AND credited_rating.user_id = ${user_bind}
    ))"
    )
}

pub(crate) fn artist_scope(user_bind: u8, libraries_bind: u8) -> String {
    let credited = artist_credited_engagement(user_bind, libraries_bind);
    let engaged_album = album_scope(AlbumListScope::Engaged, user_bind, libraries_bind);

    format!(
        "({credited}
    OR EXISTS (
        SELECT 1 FROM albums AS a
        WHERE a.album_artist_id = ar.id AND {engaged_album}
    ))"
    )
}

fn artist_membership_credit(condition: &str) -> String {
    format!(
        "SELECT membership_album.id AS album_id, membership_track.id AS track_id \
         FROM track_artist_credits AS membership_credit \
         JOIN tracks AS membership_track ON membership_track.id = membership_credit.track_id \
         LEFT JOIN album_tracks AS membership_member \
           ON membership_member.track_id = membership_track.id \
         LEFT JOIN albums AS membership_album \
           ON membership_album.id = membership_member.album_id \
         WHERE membership_credit.artist_id = ar.id{condition}"
    )
}

fn artist_membership_album_artist() -> String {
    "SELECT album_artist_album.id AS album_id, album_artist_member.track_id AS track_id \
     FROM albums AS album_artist_album \
     LEFT JOIN album_tracks AS album_artist_member \
       ON album_artist_member.album_id = album_artist_album.id \
     WHERE album_artist_album.album_artist_id = ar.id"
        .to_owned()
}

pub(crate) fn artist_membership(credit_condition: Option<&str>, album_artist: bool) -> String {
    let mut parts = Vec::new();

    if let Some(condition) = credit_condition {
        parts.push(artist_membership_credit(condition));
    }
    if album_artist {
        parts.push(artist_membership_album_artist());
    }

    parts.join(" UNION ALL ")
}
