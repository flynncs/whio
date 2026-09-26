use std::collections::HashMap;

use sqlx::PgPool;
use uuid::Uuid;

use crate::features::music_library::{
    domain::{
        Album, AlbumId, AlbumSummary, AlbumTrack, Artist, ArtistCredit, ArtistId, ArtistSummary,
        CreditRole, DatePrecision, LibraryState, PartialDate, ProviderId, SourceIdentity,
        SourceScope, Track, TrackHit, TrackId, TrackMetadata,
    },
    storage::contracts::{SourceFileFacts, TrackRepositoryError},
};

use super::schema::{
    ALBUM_BROWSE_COLUMNS, ALBUM_BROWSE_FROM, ALBUM_SEARCH_COLUMNS, AlbumBrowseRow, AlbumRow,
    ArtistRow, StateRow, TRACK_COLUMNS, TrackRecord, TrackSearchRow,
};

pub(crate) fn decode_artist(row: ArtistRow) -> Result<Artist, TrackRepositoryError> {
    let (id, name, sort_name, identifier) = row;

    Artist::new(ArtistId::from_uuid(id), name, sort_name, identifier)
        .map_err(|_| TrackRepositoryError::InvalidData)
}

pub(crate) fn decode_album(row: AlbumRow) -> Result<Album, TrackRepositoryError> {
    let (id, title, album_artist_id, year, release_type, label, group_id, release_id) = row;

    Album::new(
        AlbumId::from_uuid(id),
        title,
        album_artist_id.map(ArtistId::from_uuid),
        year.map(i32::from),
        release_type,
        label,
        group_id,
        release_id,
    )
    .map_err(|_| TrackRepositoryError::InvalidData)
}
pub(crate) fn decode_album_summary(
    row: AlbumBrowseRow,
) -> Result<AlbumSummary, TrackRepositoryError> {
    let AlbumBrowseRow {
        id,
        title,
        album_artist_id,
        year,
        release_type,
        label,
        musicbrainz_release_group_id: group_id,
        musicbrainz_release_id: release_id,
        created_at,
        artist_id,
        artist_name,
        artist_sort_name,
        artist_musicbrainz_id,
        track_count,
        duration_ms,
    } = row;

    let album = Album::new(
        AlbumId::from_uuid(id),
        title,
        album_artist_id.map(ArtistId::from_uuid),
        year.map(i32::from),
        release_type,
        label,
        group_id,
        release_id,
    )
    .map_err(|_| TrackRepositoryError::InvalidData)?;

    let album_artist = match (artist_id, artist_name) {
        (Some(id), Some(name)) => Some(
            Artist::new(
                ArtistId::from_uuid(id),
                name,
                artist_sort_name,
                artist_musicbrainz_id,
            )
            .map_err(|_| TrackRepositoryError::InvalidData)?,
        ),
        _ => None,
    };
    let track_count = u32::try_from(track_count).map_err(|_| TrackRepositoryError::InvalidData)?;

    Ok(AlbumSummary::new(
        album,
        album_artist,
        track_count,
        duration_ms.max(0) as u64,
        created_at,
    ))
}

pub(crate) fn decode_artist_summary(
    row: (Uuid, String, Option<String>, Option<String>, i64, i64),
) -> Result<ArtistSummary, TrackRepositoryError> {
    let (id, name, sort_name, identifier, album_count, song_count) = row;
    let artist = Artist::new(ArtistId::from_uuid(id), name, sort_name, identifier)
        .map_err(|_| TrackRepositoryError::InvalidData)?;
    let album_count = u32::try_from(album_count).map_err(|_| TrackRepositoryError::InvalidData)?;
    let song_count = u32::try_from(song_count).map_err(|_| TrackRepositoryError::InvalidData)?;

    Ok(ArtistSummary::new(artist, album_count).with_song_count(song_count))
}

pub(crate) fn artist_count_expressions(membership: &str) -> (String, String) {
    let album_count = format!(
        "(SELECT COUNT(DISTINCT artist_membership.album_id) \
         FROM ({membership}) AS artist_membership)"
    );
    let song_count = format!(
        "(SELECT COUNT(DISTINCT artist_membership.track_id) \
         FROM ({membership}) AS artist_membership)"
    );

    (album_count, song_count)
}
pub(crate) fn decode_track(
    record: TrackRecord,
    genres: Vec<String>,
    credits: Vec<ArtistCredit>,
) -> Result<Track, TrackRepositoryError> {
    let duration_ms = record
        .duration_ms
        .map(u64::try_from)
        .transpose()
        .map_err(|_| TrackRepositoryError::InvalidData)?;
    let recording_date = decode_date(record.recording_date, record.recording_date_precision)?;
    let original_date = decode_date(record.original_date, record.original_date_precision)?;
    let (artists, roles) = credits
        .into_iter()
        .partition::<Vec<_>, _>(|credit| credit.role().is_none());
    let metadata = TrackMetadata::new(record.title, artists, duration_ms)
        .map_err(|_| TrackRepositoryError::InvalidData)?
        .with_recording_id(record.musicbrainz_recording_id)
        .map_err(|_| TrackRepositoryError::InvalidData)?
        .with_isrc(record.isrc)
        .map_err(|_| TrackRepositoryError::InvalidData)?
        .with_genres(genres)
        .with_credits(roles)
        .with_recording_date(recording_date)
        .with_original_date(original_date);

    Ok(Track::new(TrackId::from_uuid(record.id), metadata))
}

pub(crate) fn encode_date(value: Option<PartialDate>) -> Option<chrono::NaiveDate> {
    value.map(|date| date.date())
}

pub(crate) fn encode_date_precision(value: Option<PartialDate>) -> Option<&'static str> {
    value.map(|date| date.precision().as_str())
}

pub(crate) fn decode_date(
    date: Option<chrono::NaiveDate>,
    precision: Option<String>,
) -> Result<Option<PartialDate>, TrackRepositoryError> {
    match (date, precision) {
        (None, None) => Ok(None),
        (Some(date), Some(precision)) => Ok(Some(PartialDate::new(
            date,
            DatePrecision::parse(&precision).ok_or(TrackRepositoryError::InvalidData)?,
        ))),
        _ => Err(TrackRepositoryError::InvalidData),
    }
}

pub(crate) fn decode_membership(
    disc_number: Option<i16>,
    track_number: Option<i16>,
    position: i32,
    disc_total: Option<i16>,
    track_total: Option<i16>,
) -> Result<AlbumTrack, TrackRepositoryError> {
    let position = u32::try_from(position).map_err(|_| TrackRepositoryError::InvalidData)?;

    Ok(AlbumTrack::new(
        decode_smallint(disc_number)?,
        decode_smallint(track_number)?,
        position,
    )
    .map_err(|_| TrackRepositoryError::InvalidData)?
    .with_totals(decode_smallint(disc_total)?, decode_smallint(track_total)?))
}
pub(crate) fn encode_smallint(value: Option<u16>) -> Result<Option<i16>, TrackRepositoryError> {
    value
        .map(i16::try_from)
        .transpose()
        .map_err(|_| TrackRepositoryError::InvalidData)
}

pub(crate) fn decode_smallint(value: Option<i16>) -> Result<Option<u16>, TrackRepositoryError> {
    value
        .map(u16::try_from)
        .transpose()
        .map_err(|_| TrackRepositoryError::InvalidData)
}

#[derive(sqlx::FromRow)]
pub(crate) struct SourceFileFactsRow {
    pub(crate) path: Option<String>,
    pub(crate) size_bytes: Option<i64>,
    pub(crate) modified_at: Option<chrono::DateTime<chrono::Utc>>,
    pub(crate) missing_at: Option<chrono::DateTime<chrono::Utc>>,
    pub(crate) content_type: Option<String>,
    pub(crate) suffix: Option<String>,
    pub(crate) container: Option<String>,
    pub(crate) codec: Option<String>,
    pub(crate) bitrate_kbps: Option<i32>,
    pub(crate) sample_rate_hz: Option<i32>,
    pub(crate) bit_depth: Option<i16>,
    pub(crate) channels: Option<i16>,
    pub(crate) facts_version: i16,
    pub(crate) acoustid_fingerprint: Option<String>,
}

pub(crate) fn decode_file_facts(
    row: SourceFileFactsRow,
) -> Result<SourceFileFacts, TrackRepositoryError> {
    Ok(SourceFileFacts {
        path: row.path,
        size_bytes: row.size_bytes.unwrap_or_default().max(0) as u64,
        modified_at: row.modified_at,
        missing_at: row.missing_at,
        content_type: row.content_type,
        suffix: row.suffix,
        container: row.container,
        codec: row.codec,
        bitrate_kbps: decode_integer(row.bitrate_kbps)?,
        sample_rate_hz: decode_integer(row.sample_rate_hz)?,
        bit_depth: decode_byte(row.bit_depth)?,
        channels: decode_byte(row.channels)?,
        facts_version: decode_facts_version(row.facts_version)?,
        acoustid_fingerprint: row.acoustid_fingerprint,
    })
}

#[derive(sqlx::FromRow)]
pub(crate) struct ScannedPathRow {
    pub(crate) provider_id: String,
    pub(crate) source_scope: String,
    pub(crate) external_id: String,
    #[sqlx(flatten)]
    pub(crate) facts: SourceFileFactsRow,
}

pub(crate) fn decode_scanned_path(
    row: ScannedPathRow,
) -> Result<(SourceIdentity, SourceFileFacts), TrackRepositoryError> {
    Ok((
        decode_source(row.provider_id, row.source_scope, row.external_id)?,
        decode_file_facts(row.facts)?,
    ))
}

pub(crate) fn encode_integer(value: Option<u32>) -> Result<Option<i32>, TrackRepositoryError> {
    value
        .map(i32::try_from)
        .transpose()
        .map_err(|_| TrackRepositoryError::InvalidData)
}

pub(crate) fn decode_integer(value: Option<i32>) -> Result<Option<u32>, TrackRepositoryError> {
    value
        .map(u32::try_from)
        .transpose()
        .map_err(|_| TrackRepositoryError::InvalidData)
}

pub(crate) fn encode_byte(value: Option<u8>) -> Option<i16> {
    value.map(i16::from)
}

pub(crate) fn decode_byte(value: Option<i16>) -> Result<Option<u8>, TrackRepositoryError> {
    value
        .map(u8::try_from)
        .transpose()
        .map_err(|_| TrackRepositoryError::InvalidData)
}

pub(crate) fn encode_facts_version(value: u16) -> Result<i16, TrackRepositoryError> {
    i16::try_from(value).map_err(|_| TrackRepositoryError::InvalidData)
}

pub(crate) fn decode_facts_version(value: i16) -> Result<u16, TrackRepositoryError> {
    u16::try_from(value).map_err(|_| TrackRepositoryError::InvalidData)
}

pub(crate) fn decode_source(
    provider_id: String,
    source_scope: String,
    external_id: String,
) -> Result<SourceIdentity, TrackRepositoryError> {
    let provider_id =
        ProviderId::new(provider_id).map_err(|_| TrackRepositoryError::InvalidData)?;

    let source_scope = match source_scope.as_str() {
        "global" => SourceScope::Global,
        value if value.starts_with("named:") && value.len() > "named:".len() => {
            SourceScope::Named(value["named:".len()..].to_owned())
        }
        _ => return Err(TrackRepositoryError::InvalidData),
    };

    SourceIdentity::new(provider_id, source_scope, external_id)
        .map_err(|_| TrackRepositoryError::InvalidData)
}
pub(crate) fn encode_duration_ms(
    duration_ms: Option<u64>,
) -> Result<Option<i64>, TrackRepositoryError> {
    duration_ms
        .map(i64::try_from)
        .transpose()
        .map_err(|_| TrackRepositoryError::InvalidData)
}

pub(crate) fn repository_error(error: sqlx::Error) -> TrackRepositoryError {
    if let sqlx::Error::Database(database) = &error
        && (database.is_unique_violation()
            || database.is_check_violation()
            || database.is_foreign_key_violation())
    {
        return TrackRepositoryError::InvalidData;
    }

    TrackRepositoryError::Unavailable
}

pub(crate) fn encode_source_scope(source_scope: &SourceScope) -> String {
    match source_scope {
        SourceScope::Global => "global".to_owned(),
        SourceScope::Named(name) => format!("named:{}", name),
    }
}

pub(crate) fn decode_state(
    starred_at: Option<chrono::DateTime<chrono::Utc>>,
    rating: Option<i16>,
    play_count: i64,
    last_played_at: Option<chrono::DateTime<chrono::Utc>>,
) -> Result<LibraryState, TrackRepositoryError> {
    let rating = rating
        .map(u8::try_from)
        .transpose()
        .map_err(|_| TrackRepositoryError::InvalidData)?;
    let play_count =
        u32::try_from(play_count.max(0)).map_err(|_| TrackRepositoryError::InvalidData)?;

    LibraryState::new(starred_at, rating, play_count, last_played_at)
        .map_err(|_| TrackRepositoryError::InvalidData)
}

pub(crate) async fn track_states(
    pool: &PgPool,
    user: Option<Uuid>,
    track_ids: &[Uuid],
) -> Result<HashMap<Uuid, LibraryState>, TrackRepositoryError> {
    let Some(user) = user else {
        return Ok(HashMap::new());
    };
    if track_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let rows = sqlx::query_as::<_, StateRow>(
        r#"
        SELECT t.id, fav.starred_at, rating.rating,
               COALESCE(plays.play_count, 0)::BIGINT AS play_count, plays.last_played_at
        FROM tracks AS t
        LEFT JOIN track_favorites AS fav ON fav.track_id = t.id AND fav.user_id = $1
        LEFT JOIN track_ratings AS rating ON rating.track_id = t.id AND rating.user_id = $1
        LEFT JOIN (
            SELECT track_id, SUM(play_count)::BIGINT AS play_count,
                   MAX(last_played_at) AS last_played_at
            FROM track_play_stats
            WHERE user_id = $1 AND track_id = ANY($2)
            GROUP BY track_id
        ) AS plays ON plays.track_id = t.id
        WHERE t.id = ANY($2)
        "#,
    )
    .bind(user)
    .bind(track_ids)
    .fetch_all(pool)
    .await
    .map_err(repository_error)?;

    rows.into_iter()
        .map(|(id, starred_at, rating, play_count, last_played_at)| {
            decode_state(starred_at, rating, play_count, last_played_at).map(|state| (id, state))
        })
        .collect()
}

pub(crate) async fn album_states(
    pool: &PgPool,
    user: Option<Uuid>,
    album_ids: &[Uuid],
) -> Result<HashMap<Uuid, LibraryState>, TrackRepositoryError> {
    let Some(user) = user else {
        return Ok(HashMap::new());
    };
    if album_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let rows = sqlx::query_as::<_, StateRow>(
        r#"
        SELECT a.id, fav.starred_at, rating.rating,
               COALESCE(plays.play_count, 0)::BIGINT AS play_count, plays.last_played_at
        FROM albums AS a
        LEFT JOIN album_favorites AS fav ON fav.album_id = a.id AND fav.user_id = $1
        LEFT JOIN album_ratings AS rating ON rating.album_id = a.id AND rating.user_id = $1
        LEFT JOIN (
            SELECT mt.album_id, SUM(stats.play_count)::BIGINT AS play_count,
                   MAX(stats.last_played_at) AS last_played_at
            FROM track_play_stats AS stats
            JOIN album_tracks AS mt ON mt.track_id = stats.track_id
            WHERE stats.user_id = $1 AND mt.album_id = ANY($2)
            GROUP BY mt.album_id
        ) AS plays ON plays.album_id = a.id
        WHERE a.id = ANY($2)
        "#,
    )
    .bind(user)
    .bind(album_ids)
    .fetch_all(pool)
    .await
    .map_err(repository_error)?;

    rows.into_iter()
        .map(|(id, starred_at, rating, play_count, last_played_at)| {
            decode_state(starred_at, rating, play_count, last_played_at).map(|state| (id, state))
        })
        .collect()
}

pub(crate) async fn artist_states(
    pool: &PgPool,
    user: Option<Uuid>,
    artist_ids: &[Uuid],
) -> Result<HashMap<Uuid, LibraryState>, TrackRepositoryError> {
    let Some(user) = user else {
        return Ok(HashMap::new());
    };
    if artist_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let rows = sqlx::query_as::<_, StateRow>(
        r#"
        SELECT ar.id, fav.starred_at, rating.rating,
               COALESCE(plays.play_count, 0)::BIGINT AS play_count, plays.last_played_at
        FROM artists AS ar
        LEFT JOIN artist_favorites AS fav ON fav.artist_id = ar.id AND fav.user_id = $1
        LEFT JOIN artist_ratings AS rating ON rating.artist_id = ar.id AND rating.user_id = $1
        LEFT JOIN (
            SELECT albums.album_artist_id AS artist_id,
                   SUM(stats.play_count)::BIGINT AS play_count,
                   MAX(stats.last_played_at) AS last_played_at
            FROM track_play_stats AS stats
            JOIN album_tracks AS mt ON mt.track_id = stats.track_id
            JOIN albums ON albums.id = mt.album_id
            WHERE stats.user_id = $1 AND albums.album_artist_id = ANY($2)
            GROUP BY albums.album_artist_id
        ) AS plays ON plays.artist_id = ar.id
        WHERE ar.id = ANY($2)
        "#,
    )
    .bind(user)
    .bind(artist_ids)
    .fetch_all(pool)
    .await
    .map_err(repository_error)?;

    rows.into_iter()
        .map(|(id, starred_at, rating, play_count, last_played_at)| {
            decode_state(starred_at, rating, play_count, last_played_at).map(|state| (id, state))
        })
        .collect()
}

pub(crate) fn attach_album_state(
    summary: AlbumSummary,
    states: &HashMap<Uuid, LibraryState>,
) -> AlbumSummary {
    let state = states
        .get(summary.album().id().as_uuid())
        .copied()
        .unwrap_or_default();

    summary.with_state(state)
}

pub(crate) fn attach_artist_state(
    summary: ArtistSummary,
    states: &HashMap<Uuid, LibraryState>,
) -> ArtistSummary {
    let state = states
        .get(summary.artist().id().as_uuid())
        .copied()
        .unwrap_or_default();

    summary.with_state(state)
}

pub(crate) async fn attach_album_states(
    pool: &PgPool,
    user: Option<Uuid>,
    summaries: &mut [AlbumSummary],
) -> Result<(), TrackRepositoryError> {
    if summaries.is_empty() {
        return Ok(());
    }

    let ids = summaries
        .iter()
        .map(|summary| *summary.album().id().as_uuid())
        .collect::<Vec<_>>();
    let states = album_states(pool, user, &ids).await?;
    for summary in summaries {
        let state = states
            .get(summary.album().id().as_uuid())
            .copied()
            .unwrap_or_default();
        *summary = summary.clone().with_state(state);
    }

    Ok(())
}

pub(crate) async fn attach_artist_states(
    pool: &PgPool,
    user: Option<Uuid>,
    summaries: &mut [ArtistSummary],
) -> Result<(), TrackRepositoryError> {
    if summaries.is_empty() {
        return Ok(());
    }

    let ids = summaries
        .iter()
        .map(|summary| *summary.artist().id().as_uuid())
        .collect::<Vec<_>>();
    let states = artist_states(pool, user, &ids).await?;
    for summary in summaries {
        let state = states
            .get(summary.artist().id().as_uuid())
            .copied()
            .unwrap_or_default();
        *summary = summary.clone().with_state(state);
    }

    Ok(())
}

pub(crate) async fn tracks_by_ids(
    pool: &PgPool,
    track_ids: &[Uuid],
    user: Uuid,
) -> Result<Vec<TrackHit>, TrackRepositoryError> {
    if track_ids.is_empty() {
        return Ok(Vec::new());
    }

    let rows = sqlx::query_as::<_, TrackSearchRow>(&format!(
        r#"
        SELECT {TRACK_COLUMNS},
               {ALBUM_SEARCH_COLUMNS}
        FROM tracks AS t
        LEFT JOIN album_tracks AS mt ON mt.track_id = t.id
        LEFT JOIN albums AS a ON a.id = mt.album_id
        WHERE t.id = ANY($1)
        "#
    ))
    .bind(track_ids)
    .fetch_all(pool)
    .await
    .map_err(repository_error)?;

    let hits = hits_from_rows(pool, rows, Some(user)).await?;
    let mut by_id = hits
        .into_iter()
        .map(|hit| (*hit.track().id().as_uuid(), hit))
        .collect::<HashMap<_, _>>();

    Ok(track_ids.iter().filter_map(|id| by_id.remove(id)).collect())
}

pub(crate) async fn album_summaries_by_ids(
    pool: &PgPool,
    album_ids: &[Uuid],
    user: Option<Uuid>,
) -> Result<Vec<AlbumSummary>, TrackRepositoryError> {
    if album_ids.is_empty() {
        return Ok(Vec::new());
    }

    let rows = sqlx::query_as::<_, AlbumBrowseRow>(&format!(
        r#"
        SELECT {ALBUM_BROWSE_COLUMNS}
        {ALBUM_BROWSE_FROM}
        WHERE a.id = ANY($1)
        GROUP BY a.id, ar.id
        "#
    ))
    .bind(album_ids)
    .fetch_all(pool)
    .await
    .map_err(repository_error)?;

    let states = album_states(pool, user, album_ids).await?;
    let mut by_id = rows
        .into_iter()
        .map(decode_album_summary)
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|summary| {
            (
                *summary.album().id().as_uuid(),
                attach_album_state(summary, &states),
            )
        })
        .collect::<HashMap<_, _>>();

    Ok(album_ids.iter().filter_map(|id| by_id.remove(id)).collect())
}

pub(crate) async fn artist_summaries_by_ids(
    pool: &PgPool,
    artist_ids: &[Uuid],
    user: Option<Uuid>,
) -> Result<Vec<ArtistSummary>, TrackRepositoryError> {
    if artist_ids.is_empty() {
        return Ok(Vec::new());
    }

    let rows = sqlx::query_as::<_, (Uuid, String, Option<String>, Option<String>, i64, i64)>(
        r#"
        SELECT ar.id, ar.name, ar.sort_name, ar.musicbrainz_artist_id,
               (SELECT COUNT(*) FROM albums WHERE album_artist_id = ar.id)::BIGINT,
               0::BIGINT
        FROM artists AS ar
        WHERE ar.id = ANY($1)
        "#,
    )
    .bind(artist_ids)
    .fetch_all(pool)
    .await
    .map_err(repository_error)?;

    let states = artist_states(pool, user, artist_ids).await?;
    let mut by_id = rows
        .into_iter()
        .map(decode_artist_summary)
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .map(|summary| {
            (
                *summary.artist().id().as_uuid(),
                attach_artist_state(summary, &states),
            )
        })
        .collect::<HashMap<_, _>>();

    Ok(artist_ids
        .iter()
        .filter_map(|id| by_id.remove(id))
        .collect())
}

pub(crate) async fn hits_from_rows(
    pool: &PgPool,
    rows: Vec<TrackSearchRow>,
    user: Option<Uuid>,
) -> Result<Vec<TrackHit>, TrackRepositoryError> {
    let track_ids = rows.iter().map(|row| row.track.id).collect::<Vec<_>>();
    let mut credits = credits_for_tracks(pool, &track_ids).await?;
    let mut genres = genres_for_tracks(pool, &track_ids).await?;
    let mut sources = sources_for_tracks(pool, &track_ids).await?;
    let states = track_states(pool, user, &track_ids).await?;
    let mut hits = Vec::with_capacity(rows.len());

    for row in rows {
        let TrackSearchRow {
            track,
            album_id,
            album_title,
            album_artist_id,
            year,
            release_type,
            label,
            musicbrainz_release_group_id: group_id,
            musicbrainz_release_id: release_id,
        } = row;
        let id = track.id;

        let track = decode_track(
            track,
            genres.remove(&id).unwrap_or_default(),
            credits.remove(&id).unwrap_or_default(),
        )?;
        let album = match (album_id, album_title) {
            (Some(album_id), Some(album_title)) => Some(
                Album::new(
                    AlbumId::from_uuid(album_id),
                    album_title,
                    album_artist_id.map(ArtistId::from_uuid),
                    year.map(i32::from),
                    release_type,
                    label,
                    group_id,
                    release_id,
                )
                .map_err(|_| TrackRepositoryError::InvalidData)?,
            ),
            _ => None,
        };
        let state = states.get(&id).copied().unwrap_or_default();

        hits.push(
            TrackHit::new(track, album, sources.remove(&id).unwrap_or_default()).with_state(state),
        );
    }

    Ok(hits)
}

pub(crate) async fn credits_for_tracks(
    pool: &PgPool,
    track_ids: &[Uuid],
) -> Result<HashMap<Uuid, Vec<ArtistCredit>>, TrackRepositoryError> {
    if track_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let rows = sqlx::query_as::<_, (Uuid, String, Option<String>, Option<String>)>(
        r#"
        SELECT credits.track_id, credits.name, artists.musicbrainz_artist_id, credits.role
        FROM track_artist_credits AS credits
        LEFT JOIN artists ON artists.id = credits.artist_id
        WHERE credits.track_id = ANY($1)
        ORDER BY credits.track_id, credits.position
        "#,
    )
    .bind(track_ids)
    .fetch_all(pool)
    .await
    .map_err(repository_error)?;

    let mut by_track: HashMap<Uuid, Vec<ArtistCredit>> = HashMap::new();
    for (track_id, name, identifier, role) in rows {
        let credit = match role {
            Some(role) => ArtistCredit::new(name, identifier)
                .map_err(|_| TrackRepositoryError::InvalidData)?
                .with_role(CreditRole::parse(&role).ok_or(TrackRepositoryError::InvalidData)?),
            None => ArtistCredit::new(name, identifier)
                .map_err(|_| TrackRepositoryError::InvalidData)?,
        };

        by_track.entry(track_id).or_default().push(credit);
    }

    Ok(by_track)
}

pub(crate) async fn genres_for_tracks(
    pool: &PgPool,
    track_ids: &[Uuid],
) -> Result<HashMap<Uuid, Vec<String>>, TrackRepositoryError> {
    if track_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let rows = sqlx::query_as::<_, (Uuid, String)>(
        r#"
        SELECT track_id, name
        FROM track_genres
        WHERE track_id = ANY($1)
        ORDER BY track_id, position
        "#,
    )
    .bind(track_ids)
    .fetch_all(pool)
    .await
    .map_err(repository_error)?;

    let mut by_track: HashMap<Uuid, Vec<String>> = HashMap::new();
    for (track_id, name) in rows {
        by_track.entry(track_id).or_default().push(name);
    }

    Ok(by_track)
}

pub(crate) async fn sources_for_tracks(
    pool: &PgPool,
    track_ids: &[Uuid],
) -> Result<HashMap<Uuid, Vec<SourceIdentity>>, TrackRepositoryError> {
    if track_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let rows = sqlx::query_as::<_, (Uuid, String, String, String)>(
        r#"
        SELECT track_id, provider_id, source_scope, external_id
        FROM track_sources
        WHERE track_id = ANY($1)
        ORDER BY created_at, id
        "#,
    )
    .bind(track_ids)
    .fetch_all(pool)
    .await
    .map_err(repository_error)?;

    let mut by_track: HashMap<Uuid, Vec<SourceIdentity>> = HashMap::new();
    for (track_id, provider_id, source_scope, external_id) in rows {
        let source = decode_source(provider_id, source_scope, external_id)?;

        by_track.entry(track_id).or_default().push(source);
    }

    Ok(by_track)
}

pub(crate) async fn albums_for_tracks(
    pool: &PgPool,
    track_ids: &[Uuid],
) -> Result<HashMap<Uuid, (Album, AlbumTrack, Option<String>)>, TrackRepositoryError> {
    if track_ids.is_empty() {
        return Ok(HashMap::new());
    }

    let rows = sqlx::query_as::<
        _,
        (
            Uuid,
            Uuid,
            String,
            Option<Uuid>,
            Option<i16>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
            Option<i16>,
            Option<i16>,
            i32,
            Option<i16>,
            Option<i16>,
            Option<String>,
        ),
    >(
        r#"
        SELECT mt.track_id, a.id, a.title, a.album_artist_id, a.year, a.release_type, a.label,
               a.musicbrainz_release_group_id, a.musicbrainz_release_id,
               mt.disc_number, mt.track_number, mt.position, mt.disc_total, mt.track_total,
               album_artist.name
        FROM album_tracks AS mt
        JOIN albums AS a ON a.id = mt.album_id
        LEFT JOIN artists AS album_artist ON album_artist.id = a.album_artist_id
        WHERE mt.track_id = ANY($1)
        ORDER BY mt.track_id, a.title_normalized, a.id
        "#,
    )
    .bind(track_ids)
    .fetch_all(pool)
    .await
    .map_err(repository_error)?;

    let mut by_track: HashMap<Uuid, (Album, AlbumTrack, Option<String>)> = HashMap::new();
    for (
        track_id,
        album_id,
        title,
        album_artist_id,
        year,
        release_type,
        label,
        group_id,
        release_id,
        disc_number,
        track_number,
        position,
        disc_total,
        track_total,
        album_artist,
    ) in rows
    {
        let album = decode_album((
            album_id,
            title,
            album_artist_id,
            year,
            release_type,
            label,
            group_id,
            release_id,
        ))?;
        let membership =
            decode_membership(disc_number, track_number, position, disc_total, track_total)?;

        by_track
            .entry(track_id)
            .or_insert_with(|| (album, membership, album_artist));
    }

    Ok(by_track)
}
