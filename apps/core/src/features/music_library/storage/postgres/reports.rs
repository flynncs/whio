use async_trait::async_trait;
use uuid::Uuid;

use crate::features::music_library::{
    domain::{
        ArtistCredit, ArtistId, ArtistMerge, DuplicateCandidate, DuplicateKind, DuplicateSource,
        EvictionReport, LOCAL_PROVIDER_ID, ProviderDuplicate, ProviderDuplicateReport,
        RetentionReport, SkipReason, SkippedDuplicate, SplitArtist, SplitArtistRow, TrackId,
        TrackMetadata,
    },
    storage::contracts::TrackRepositoryError,
};

use crate::features::music_library::storage::contracts::{DuplicateReport, Retention};

use super::PgStore;
use super::catalogue::{
    find_provider_track_match, find_unique_artist_by_name, resolve_cleanup_artist,
};
use super::decode::{decode_source, repository_error};
use sqlx::{PgConnection, PgPool};

fn track_engaged(id: &str) -> String {
    format!(
        "(EXISTS (SELECT 1 FROM track_sources AS ts WHERE ts.track_id = {id} AND ts.provider_id = ANY($1))
          OR EXISTS (SELECT 1 FROM track_favorites AS tf WHERE tf.track_id = {id})
          OR EXISTS (SELECT 1 FROM track_ratings AS tr WHERE tr.track_id = {id})
          OR EXISTS (SELECT 1 FROM track_play_stats AS tp WHERE tp.track_id = {id} AND tp.play_count > 0)
          OR EXISTS (SELECT 1 FROM playlist_entries AS pe WHERE pe.track_id = {id})
          OR EXISTS (SELECT 1 FROM restore_queue_entries AS queued WHERE queued.track_id = {id}))"
    )
}

fn track_engaged_without_plays(id: &str) -> String {
    format!(
        "(EXISTS (SELECT 1 FROM track_sources AS ts WHERE ts.track_id = {id} AND ts.provider_id = ANY($1))
          OR EXISTS (SELECT 1 FROM track_favorites AS tf WHERE tf.track_id = {id})
          OR EXISTS (SELECT 1 FROM track_ratings AS tr WHERE tr.track_id = {id})
          OR EXISTS (SELECT 1 FROM playlist_entries AS pe WHERE pe.track_id = {id})
          OR EXISTS (SELECT 1 FROM restore_queue_entries AS queued WHERE queued.track_id = {id}))"
    )
}

fn album_unengaged(member_engaged: &str) -> String {
    format!(
        "(NOT EXISTS (SELECT 1 FROM album_tracks AS amt WHERE amt.album_id = a.id AND {member_engaged})
          AND NOT EXISTS (SELECT 1 FROM album_favorites AS af WHERE af.album_id = a.id)
          AND NOT EXISTS (SELECT 1 FROM album_ratings AS arv WHERE arv.album_id = a.id))"
    )
}

fn artist_unengaged(member_engaged: &str) -> String {
    format!(
        "(NOT EXISTS (SELECT 1 FROM track_artist_credits AS ac WHERE ac.artist_id = ar.id AND {member_engaged})
          AND NOT EXISTS (SELECT 1 FROM artist_favorites AS af WHERE af.artist_id = ar.id)
          AND NOT EXISTS (SELECT 1 FROM artist_ratings AS arv WHERE arv.artist_id = ar.id))"
    )
}

#[derive(sqlx::FromRow)]
struct TrackDuplicateCandidate {
    id: Uuid,
    title: String,
    duration_ms: Option<i64>,
    primary_artist_id: Uuid,
    artist_names: Vec<String>,
    provider_id: String,
    source_scope: String,
    external_id: String,
    listens: i64,
}

#[derive(sqlx::FromRow)]
struct AlbumDuplicateCandidate {
    id: Uuid,
    title: String,
    album_artist_id: Option<Uuid>,
}

async fn provider_duplicate_report(
    connection: &mut PgConnection,
    library_ids: &[String],
) -> Result<ProviderDuplicateReport, TrackRepositoryError> {
    let candidate_artist_ids = cleanup_candidate_artist_ids(connection, library_ids).await?;
    let mut report = ProviderDuplicateReport::default();
    let track_ids = collect_provider_track_duplicates(
        connection,
        library_ids,
        &candidate_artist_ids,
        &mut report,
    )
    .await?;
    let candidate_album_ids = cleanup_candidate_album_ids(connection, library_ids).await?;
    let deletable_album_candidates =
        deletable_provider_album_ids(connection, &candidate_album_ids, &track_ids).await?;
    let album_ids = collect_provider_album_duplicates(
        connection,
        &candidate_artist_ids,
        &candidate_album_ids,
        &deletable_album_candidates,
        &mut report,
    )
    .await?;
    collect_provider_artist_duplicates(
        connection,
        library_ids,
        &candidate_artist_ids,
        &track_ids,
        &album_ids,
        &mut report,
    )
    .await?;
    Ok(report)
}

async fn merge_artist_namesakes(
    connection: &mut PgConnection,
    library_ids: &[String],
) -> Result<Vec<ArtistMerge>, TrackRepositoryError> {
    let candidate_ids = cleanup_candidate_artist_ids(connection, library_ids).await?;
    let rows = sqlx::query_as::<_, (Uuid, String)>(
        r#"
        SELECT id, name
        FROM artists
        WHERE musicbrainz_artist_id IS NULL
          AND id <> ALL($1::UUID[])
        ORDER BY id
        "#,
    )
    .bind(&candidate_ids)
    .fetch_all(&mut *connection)
    .await
    .map_err(repository_error)?;

    let mut merges = Vec::new();
    for (id, name) in rows {
        let Some(namesake) = find_unique_artist_by_name(connection, &name, &[]).await? else {
            continue;
        };
        if namesake.musicbrainz_artist_id().is_none() {
            continue;
        }
        merges.push(merge_artist(connection, id, &name, *namesake.id().as_uuid()).await?);
    }
    Ok(merges)
}

async fn merge_artist(
    connection: &mut PgConnection,
    source_id: Uuid,
    name: &str,
    target_id: Uuid,
) -> Result<ArtistMerge, TrackRepositoryError> {
    let credits =
        sqlx::query("UPDATE track_artist_credits SET artist_id = $1 WHERE artist_id = $2")
            .bind(target_id)
            .bind(source_id)
            .execute(&mut *connection)
            .await
            .map_err(repository_error)?
            .rows_affected();

    sqlx::query("UPDATE artist_sources SET artist_id = $1 WHERE artist_id = $2")
        .bind(target_id)
        .bind(source_id)
        .execute(&mut *connection)
        .await
        .map_err(repository_error)?;

    move_state_rows(
        connection,
        "artist_favorites",
        "artist_id",
        source_id,
        target_id,
    )
    .await?;
    move_state_rows(
        connection,
        "artist_ratings",
        "artist_id",
        source_id,
        target_id,
    )
    .await?;

    let (albums_moved, albums_merged) =
        move_or_merge_albums(connection, source_id, target_id).await?;

    sqlx::query("DELETE FROM artists WHERE id = $1")
        .bind(source_id)
        .execute(&mut *connection)
        .await
        .map_err(repository_error)?;

    Ok(ArtistMerge {
        id: source_id,
        name: name.to_owned(),
        into: target_id,
        credits,
        albums_moved,
        albums_merged,
    })
}

async fn move_state_rows(
    connection: &mut PgConnection,
    table: &str,
    column: &str,
    source_id: Uuid,
    target_id: Uuid,
) -> Result<(), TrackRepositoryError> {
    sqlx::query(&format!(
        "DELETE FROM {table} AS moved
         WHERE moved.{column} = $1
           AND EXISTS (
               SELECT 1 FROM {table} AS kept
               WHERE kept.{column} = $2 AND kept.user_id = moved.user_id
           )"
    ))
    .bind(source_id)
    .bind(target_id)
    .execute(&mut *connection)
    .await
    .map_err(repository_error)?;

    sqlx::query(&format!(
        "UPDATE {table} SET {column} = $1 WHERE {column} = $2"
    ))
    .bind(target_id)
    .bind(source_id)
    .execute(&mut *connection)
    .await
    .map_err(repository_error)?;

    Ok(())
}

async fn move_or_merge_albums(
    connection: &mut PgConnection,
    source_artist_id: Uuid,
    target_artist_id: Uuid,
) -> Result<(u64, u64), TrackRepositoryError> {
    let collidable = sqlx::query_as::<_, (Uuid, String)>(
        r#"
        SELECT id, title_normalized
        FROM albums
        WHERE album_artist_id = $1
          AND musicbrainz_release_group_id IS NULL
        ORDER BY id
        "#,
    )
    .bind(source_artist_id)
    .fetch_all(&mut *connection)
    .await
    .map_err(repository_error)?;

    let mut merged = 0u64;
    for (album_id, title_normalized) in collidable {
        let Some(target_album_id) =
            find_album_under_artist(connection, target_artist_id, &title_normalized).await?
        else {
            continue;
        };
        merge_album(connection, album_id, target_album_id).await?;
        merged += 1;
    }

    let moved = sqlx::query("UPDATE albums SET album_artist_id = $1 WHERE album_artist_id = $2")
        .bind(target_artist_id)
        .bind(source_artist_id)
        .execute(&mut *connection)
        .await
        .map_err(repository_error)?
        .rows_affected();

    Ok((moved, merged))
}

async fn find_album_under_artist(
    connection: &mut PgConnection,
    artist_id: Uuid,
    title_normalized: &str,
) -> Result<Option<Uuid>, TrackRepositoryError> {
    sqlx::query_scalar(
        r#"
        SELECT id
        FROM albums
        WHERE album_artist_id = $1
          AND title_normalized = $2
          AND musicbrainz_release_group_id IS NULL
        LIMIT 1
        "#,
    )
    .bind(artist_id)
    .bind(title_normalized)
    .fetch_optional(&mut *connection)
    .await
    .map_err(repository_error)
}

async fn merge_album(
    connection: &mut PgConnection,
    source_album_id: Uuid,
    target_album_id: Uuid,
) -> Result<(), TrackRepositoryError> {
    sqlx::query(
        r#"
        DELETE FROM album_tracks AS moved
        WHERE moved.album_id = $1
          AND EXISTS (
              SELECT 1 FROM album_tracks AS kept
              WHERE kept.album_id = $2 AND kept.track_id = moved.track_id
          )
        "#,
    )
    .bind(source_album_id)
    .bind(target_album_id)
    .execute(&mut *connection)
    .await
    .map_err(repository_error)?;

    sqlx::query("UPDATE album_tracks SET album_id = $1 WHERE album_id = $2")
        .bind(target_album_id)
        .bind(source_album_id)
        .execute(&mut *connection)
        .await
        .map_err(repository_error)?;

    sqlx::query("UPDATE album_sources SET album_id = $1 WHERE album_id = $2")
        .bind(target_album_id)
        .bind(source_album_id)
        .execute(&mut *connection)
        .await
        .map_err(repository_error)?;

    move_state_rows(
        connection,
        "album_favorites",
        "album_id",
        source_album_id,
        target_album_id,
    )
    .await?;
    move_state_rows(
        connection,
        "album_ratings",
        "album_id",
        source_album_id,
        target_album_id,
    )
    .await?;

    sqlx::query("DELETE FROM albums WHERE id = $1")
        .bind(source_album_id)
        .execute(&mut *connection)
        .await
        .map_err(repository_error)?;

    Ok(())
}

async fn delete_reported_duplicates(
    connection: &mut PgConnection,
    report: &ProviderDuplicateReport,
) -> Result<(), TrackRepositoryError> {
    let ids_of = |kind| {
        report
            .duplicates
            .iter()
            .filter(|duplicate| duplicate.kind == kind)
            .map(|duplicate| duplicate.id)
            .collect::<Vec<_>>()
    };
    let track_ids = ids_of(DuplicateKind::Track);
    let album_ids = ids_of(DuplicateKind::Album);
    let artist_ids = ids_of(DuplicateKind::Artist);

    let tracks = delete_provider_duplicate_rows(connection, "tracks", &track_ids).await?;
    let albums = delete_provider_duplicate_rows(connection, "albums", &album_ids).await?;
    let artists = delete_provider_duplicate_rows(connection, "artists", &artist_ids).await?;
    if tracks != track_ids.len() as u64
        || albums != album_ids.len() as u64
        || artists != artist_ids.len() as u64
    {
        return Err(TrackRepositoryError::InvalidData);
    }

    Ok(())
}

async fn cleanup_candidate_artist_ids(
    connection: &mut PgConnection,
    library_ids: &[String],
) -> Result<Vec<Uuid>, TrackRepositoryError> {
    sqlx::query_scalar(
        r#"
        SELECT artist.id
        FROM artists AS artist
        WHERE EXISTS (
            SELECT 1 FROM artist_sources AS source
            WHERE source.artist_id = artist.id
              AND source.provider_id <> ALL($1::TEXT[])
        )
          AND NOT EXISTS (
            SELECT 1 FROM artist_sources AS source
            WHERE source.artist_id = artist.id
              AND source.provider_id = ANY($1::TEXT[])
        )
          AND NOT EXISTS (
            SELECT 1
            FROM track_artist_credits AS credit
            JOIN track_sources AS source ON source.track_id = credit.track_id
            WHERE credit.artist_id = artist.id
              AND source.provider_id = ANY($1::TEXT[])
        )
          AND NOT EXISTS (
            SELECT 1
            FROM albums AS album
            WHERE album.album_artist_id = artist.id
              AND (
                  EXISTS (
                      SELECT 1 FROM album_sources AS source
                      WHERE source.album_id = album.id
                        AND source.provider_id = ANY($1::TEXT[])
                  )
                  OR EXISTS (
                      SELECT 1
                      FROM album_tracks AS membership
                      JOIN track_sources AS source ON source.track_id = membership.track_id
                      WHERE membership.album_id = album.id
                        AND source.provider_id = ANY($1::TEXT[])
                  )
              )
        )
        ORDER BY artist.id
        "#,
    )
    .bind(library_ids)
    .fetch_all(&mut *connection)
    .await
    .map_err(repository_error)
}

async fn collect_provider_track_duplicates(
    connection: &mut PgConnection,
    library_ids: &[String],
    candidate_artist_ids: &[Uuid],
    report: &mut ProviderDuplicateReport,
) -> Result<Vec<Uuid>, TrackRepositoryError> {
    let rows = sqlx::query_as::<_, TrackDuplicateCandidate>(
        r#"
        SELECT t.id, t.title, t.duration_ms,
               primary_credit.artist_id AS primary_artist_id,
               ARRAY(
                   SELECT credit.name FROM track_artist_credits AS credit
                   WHERE credit.track_id = t.id AND credit.role IS NULL
                   ORDER BY credit.position
               ) AS artist_names,
               source.provider_id, source.source_scope, source.external_id,
               (SELECT count(*) FROM listens WHERE track_id = t.id) AS listens
        FROM tracks AS t
        JOIN track_artist_credits AS primary_credit
          ON primary_credit.track_id = t.id
         AND primary_credit.position = 0
         AND primary_credit.role IS NULL
        JOIN LATERAL (
            SELECT provider_id, source_scope, external_id
            FROM track_sources
            WHERE track_id = t.id AND provider_id <> ALL($1::TEXT[])
            ORDER BY provider_id, source_scope, external_id
            LIMIT 1
        ) AS source ON TRUE
        WHERE t.musicbrainz_recording_id IS NULL AND t.isrc IS NULL
          AND NOT EXISTS (
              SELECT 1 FROM track_sources AS library_source
              WHERE library_source.track_id = t.id
                AND library_source.provider_id = ANY($1::TEXT[])
          )
        ORDER BY t.id
        "#,
    )
    .bind(library_ids)
    .fetch_all(&mut *connection)
    .await
    .map_err(repository_error)?;

    let mut deletable_ids = Vec::new();
    for row in rows {
        let source = decode_source(row.provider_id, row.source_scope, row.external_id)?;
        let Some(primary_artist_id) =
            resolve_cleanup_artist(connection, row.primary_artist_id, candidate_artist_ids).await?
        else {
            continue;
        };
        let artists = row
            .artist_names
            .iter()
            .map(|name| {
                ArtistCredit::new(name.clone(), None).map_err(|_| TrackRepositoryError::InvalidData)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let metadata = TrackMetadata::new(
            row.title.clone(),
            artists,
            row.duration_ms.and_then(|value| u64::try_from(value).ok()),
        )
        .map_err(|_| TrackRepositoryError::InvalidData)?;
        let credit = super::catalogue::ResolvedCredit {
            name: metadata
                .artists()
                .first()
                .map(|artist| artist.name().to_owned())
                .unwrap_or_default(),
            artist_id: primary_artist_id,
            role: None,
        };
        let Some(duplicate_of) = find_provider_track_match(
            connection,
            &source,
            &metadata,
            std::slice::from_ref(&credit),
            library_ids,
        )
        .await?
        else {
            continue;
        };

        if let Some(reason) = track_skip_reason(connection, row.id).await? {
            report.skipped.push(SkippedDuplicate {
                kind: DuplicateKind::Track,
                id: row.id,
                name: row.title,
                reason,
            });
            continue;
        }

        deletable_ids.push(row.id);
        report.duplicates.push(ProviderDuplicate {
            kind: DuplicateKind::Track,
            id: row.id,
            name: row.title,
            duplicate_of: *duplicate_of.as_uuid(),
            listens: row.listens.max(0) as u64,
        });
    }
    Ok(deletable_ids)
}

async fn cleanup_candidate_album_ids(
    connection: &mut PgConnection,
    library_ids: &[String],
) -> Result<Vec<Uuid>, TrackRepositoryError> {
    sqlx::query_scalar(
        r#"
        SELECT album.id
        FROM albums AS album
        WHERE EXISTS (
            SELECT 1 FROM album_sources AS source
            WHERE source.album_id = album.id
              AND source.provider_id <> ALL($1::TEXT[])
        )
          AND NOT EXISTS (
            SELECT 1 FROM album_sources AS source
            WHERE source.album_id = album.id
              AND source.provider_id = ANY($1::TEXT[])
        )
          AND NOT EXISTS (
            SELECT 1
            FROM album_tracks AS membership
            JOIN track_sources AS source ON source.track_id = membership.track_id
            WHERE membership.album_id = album.id
              AND source.provider_id = ANY($1::TEXT[])
        )
        ORDER BY album.id
        "#,
    )
    .bind(library_ids)
    .fetch_all(&mut *connection)
    .await
    .map_err(repository_error)
}

async fn deletable_provider_album_ids(
    connection: &mut PgConnection,
    candidate_album_ids: &[Uuid],
    deletable_track_ids: &[Uuid],
) -> Result<Vec<Uuid>, TrackRepositoryError> {
    sqlx::query_scalar(
        r#"
        SELECT album.id
        FROM albums AS album
        WHERE album.id = ANY($1::UUID[])
          AND album.musicbrainz_release_group_id IS NULL
          AND album.musicbrainz_release_id IS NULL
          AND NOT EXISTS (
              SELECT 1 FROM album_tracks AS membership
              WHERE membership.album_id = album.id
                AND membership.track_id <> ALL($2::UUID[])
          )
        ORDER BY album.id
        "#,
    )
    .bind(candidate_album_ids)
    .bind(deletable_track_ids)
    .fetch_all(&mut *connection)
    .await
    .map_err(repository_error)
}

async fn collect_provider_album_duplicates(
    connection: &mut PgConnection,
    candidate_artist_ids: &[Uuid],
    candidate_album_ids: &[Uuid],
    deletable_album_candidates: &[Uuid],
    report: &mut ProviderDuplicateReport,
) -> Result<Vec<Uuid>, TrackRepositoryError> {
    if candidate_album_ids.is_empty() {
        return Ok(Vec::new());
    }
    let rows = sqlx::query_as::<_, AlbumDuplicateCandidate>(
        r#"
        SELECT album.id, album.title, album.album_artist_id
        FROM albums AS album
        WHERE album.id = ANY($1::UUID[])
        ORDER BY album.id
        "#,
    )
    .bind(deletable_album_candidates)
    .fetch_all(&mut *connection)
    .await
    .map_err(repository_error)?;

    let mut deletable_ids = Vec::new();
    for row in rows {
        let resolved_artist_id = match row.album_artist_id {
            Some(id) => {
                let Some(artist_id) =
                    resolve_cleanup_artist(connection, id, candidate_artist_ids).await?
                else {
                    continue;
                };
                Some(artist_id)
            }
            None => None,
        };
        let Some(duplicate) = super::catalogue::find_unique_library_album(
            connection,
            &row.title,
            resolved_artist_id.as_ref(),
            candidate_album_ids,
        )
        .await?
        else {
            continue;
        };
        if duplicate.id().as_uuid() == &row.id {
            continue;
        }
        deletable_ids.push(row.id);
        report.duplicates.push(ProviderDuplicate {
            kind: DuplicateKind::Album,
            id: row.id,
            name: row.title,
            duplicate_of: *duplicate.id().as_uuid(),
            listens: 0,
        });
    }
    Ok(deletable_ids)
}

async fn collect_provider_artist_duplicates(
    connection: &mut PgConnection,
    library_ids: &[String],
    candidate_artist_ids: &[Uuid],
    deletable_track_ids: &[Uuid],
    deletable_album_ids: &[Uuid],
    report: &mut ProviderDuplicateReport,
) -> Result<(), TrackRepositoryError> {
    let rows = sqlx::query_as::<_, (Uuid, String)>(
        r#"
        SELECT artist.id, artist.name
        FROM artists AS artist
        WHERE artist.musicbrainz_artist_id IS NULL
          AND artist.id = ANY($2::UUID[])
          AND EXISTS (
              SELECT 1 FROM artist_sources AS source
              WHERE source.artist_id = artist.id
                AND source.provider_id <> ALL($1::TEXT[])
          )
          AND NOT EXISTS (
              SELECT 1 FROM artist_sources AS source
              WHERE source.artist_id = artist.id
                AND source.provider_id = ANY($1::TEXT[])
          )
          AND NOT EXISTS (
              SELECT 1 FROM track_artist_credits AS credit
              WHERE credit.artist_id = artist.id
                AND credit.track_id <> ALL($3::UUID[])
          )
          AND NOT EXISTS (
              SELECT 1 FROM albums AS album
              WHERE album.album_artist_id = artist.id
                AND album.id <> ALL($4::UUID[])
          )
        ORDER BY artist.id
        "#,
    )
    .bind(library_ids)
    .bind(candidate_artist_ids)
    .bind(deletable_track_ids)
    .bind(deletable_album_ids)
    .fetch_all(&mut *connection)
    .await
    .map_err(repository_error)?;

    for (id, name) in rows {
        let Some(duplicate) =
            find_unique_artist_by_name(connection, &name, candidate_artist_ids).await?
        else {
            continue;
        };
        report.duplicates.push(ProviderDuplicate {
            kind: DuplicateKind::Artist,
            id,
            name,
            duplicate_of: *duplicate.id().as_uuid(),
            listens: 0,
        });
    }
    Ok(())
}

async fn delete_provider_duplicate_rows(
    connection: &mut PgConnection,
    table: &str,
    ids: &[Uuid],
) -> Result<u64, TrackRepositoryError> {
    sqlx::query(&format!("DELETE FROM {table} WHERE id = ANY($1::UUID[])"))
        .bind(ids)
        .execute(&mut *connection)
        .await
        .map(|result| result.rows_affected())
        .map_err(repository_error)
}

async fn track_skip_reason(
    connection: &mut PgConnection,
    track_id: Uuid,
) -> Result<Option<SkipReason>, TrackRepositoryError> {
    let playlist = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (SELECT 1 FROM playlist_entries WHERE track_id = $1)",
    )
    .bind(track_id)
    .fetch_one(&mut *connection)
    .await
    .map_err(repository_error)?;
    if playlist {
        return Ok(Some(SkipReason::PlaylistEntry));
    }

    let queued = sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (SELECT 1 FROM restore_queue_entries WHERE track_id = $1)",
    )
    .bind(track_id)
    .fetch_one(&mut *connection)
    .await
    .map_err(repository_error)?;
    Ok(queued.then_some(SkipReason::QueueEntry))
}

#[async_trait]
impl Retention for PgStore {
    async fn retention_report(&self) -> Result<RetentionReport, TrackRepositoryError> {
        let engaged = track_engaged("t.id");
        let engaged_without_plays = track_engaged_without_plays("t.id");
        let album_evictable = album_unengaged(&track_engaged("amt.track_id"));
        let album_play_held = album_unengaged(&track_engaged_without_plays("amt.track_id"));
        let artist_evictable = artist_unengaged(&track_engaged("ac.track_id"));
        let artist_play_held = artist_unengaged(&track_engaged_without_plays("ac.track_id"));

        let query = format!(
            r#"
            SELECT
                (SELECT count(*) FROM tracks AS t WHERE NOT {engaged})::BIGINT,
                (SELECT count(*) FROM albums AS a WHERE {album_evictable})::BIGINT,
                (SELECT count(*) FROM artists AS ar WHERE {artist_evictable})::BIGINT,
                (SELECT count(*) FROM tracks AS t
                 WHERE NOT {engaged_without_plays}
                   AND EXISTS (SELECT 1 FROM track_play_stats AS tp
                               WHERE tp.track_id = t.id AND tp.play_count > 0))::BIGINT,
                (SELECT count(*) FROM albums AS a
                 WHERE {album_play_held}
                   AND EXISTS (SELECT 1 FROM album_tracks AS amt
                               JOIN track_play_stats AS tp
                                 ON tp.track_id = amt.track_id AND tp.play_count > 0
                               WHERE amt.album_id = a.id))::BIGINT,
                (SELECT count(*) FROM artists AS ar
                 WHERE {artist_play_held}
                   AND EXISTS (SELECT 1 FROM track_artist_credits AS ac
                               JOIN track_play_stats AS tp
                                 ON tp.track_id = ac.track_id AND tp.play_count > 0
                               WHERE ac.artist_id = ar.id))::BIGINT,
                (SELECT min(tp.last_played_at) FROM track_play_stats AS tp
                 JOIN tracks AS t ON t.id = tp.track_id
                 WHERE tp.play_count > 0 AND NOT {engaged_without_plays}),
                (SELECT max(tp.last_played_at) FROM track_play_stats AS tp
                 JOIN tracks AS t ON t.id = tp.track_id
                 WHERE tp.play_count > 0 AND NOT {engaged_without_plays})
            "#
        );

        let row = sqlx::query_as::<
            _,
            (
                i64,
                i64,
                i64,
                i64,
                i64,
                i64,
                Option<chrono::DateTime<chrono::Utc>>,
                Option<chrono::DateTime<chrono::Utc>>,
            ),
        >(&query)
        .bind(self.library_provider_ids())
        .fetch_one(&self.pool)
        .await
        .map_err(repository_error)?;

        Ok(RetentionReport {
            evictable_tracks: row.0.max(0) as u64,
            evictable_albums: row.1.max(0) as u64,
            evictable_artists: row.2.max(0) as u64,
            play_held_tracks: row.3.max(0) as u64,
            play_held_albums: row.4.max(0) as u64,
            play_held_artists: row.5.max(0) as u64,
            oldest_played_at: row.6,
            newest_played_at: row.7,
        })
    }

    async fn evict_unengaged(&self) -> Result<EvictionReport, TrackRepositoryError> {
        let library_ids = self.library_provider_ids();
        let mut transaction = self.pool.begin().await.map_err(repository_error)?;

        let tracks = sqlx::query(&format!(
            "DELETE FROM tracks AS t WHERE NOT {}",
            track_engaged("t.id")
        ))
        .bind(&library_ids)
        .execute(&mut *transaction)
        .await
        .map_err(repository_error)?
        .rows_affected();

        let albums = sqlx::query(&format!(
            "DELETE FROM albums AS a WHERE {}",
            album_unengaged(&track_engaged("amt.track_id"))
        ))
        .bind(&library_ids)
        .execute(&mut *transaction)
        .await
        .map_err(repository_error)?
        .rows_affected();

        let artists = sqlx::query_scalar::<_, Uuid>(&format!(
            r#"
            WITH evicted AS (
                DELETE FROM artists AS ar WHERE {predicate} RETURNING ar.id
            )
            SELECT id FROM evicted ORDER BY id
            "#,
            predicate = artist_unengaged(&track_engaged("ac.track_id"))
        ))
        .bind(&library_ids)
        .fetch_all(&mut *transaction)
        .await
        .map_err(repository_error)?;

        transaction.commit().await.map_err(repository_error)?;

        Ok(EvictionReport {
            tracks,
            albums,
            artists: artists.len() as u64,
            evicted_artist_ids: artists,
        })
    }

    async fn provider_duplicates(&self) -> Result<ProviderDuplicateReport, TrackRepositoryError> {
        run_provider_duplicate_cleanup(&self.pool, &self.library_provider_ids(), false).await
    }

    async fn delete_provider_duplicates(
        &self,
    ) -> Result<ProviderDuplicateReport, TrackRepositoryError> {
        run_provider_duplicate_cleanup(&self.pool, &self.library_provider_ids(), true).await
    }
}

async fn run_provider_duplicate_cleanup(
    pool: &PgPool,
    library_ids: &[String],
    apply: bool,
) -> Result<ProviderDuplicateReport, TrackRepositoryError> {
    let mut transaction = pool.begin().await.map_err(repository_error)?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL SERIALIZABLE")
        .execute(&mut *transaction)
        .await
        .map_err(repository_error)?;

    let merges = merge_artist_namesakes(&mut transaction, library_ids).await?;
    let mut report = provider_duplicate_report(&mut transaction, library_ids).await?;
    report.merges = merges;
    delete_reported_duplicates(&mut transaction, &report).await?;

    if apply {
        transaction.commit().await.map_err(repository_error)?;
    } else {
        transaction.rollback().await.map_err(repository_error)?;
    }

    Ok(report)
}

#[async_trait]
impl DuplicateReport for PgStore {
    async fn duplicate_candidates(
        &self,
        limit: u32,
    ) -> Result<Vec<DuplicateCandidate>, TrackRepositoryError> {
        let rows = sqlx::query_as::<
            _,
            (
                Uuid,
                String,
                Option<i64>,
                String,
                String,
                String,
                String,
                String,
                Option<i64>,
                Option<String>,
            ),
        >(
            r#"
            WITH duplicated AS (
                SELECT track_id
                FROM track_sources
                WHERE provider_id = $2
                GROUP BY track_id
                HAVING count(*) > 1
                ORDER BY track_id
                LIMIT $1
            )
            SELECT t.id, t.title, t.duration_ms,
                   coalesce((
                       SELECT string_agg(credits.name, '; ' ORDER BY credits.position)
                       FROM track_artist_credits AS credits
                       WHERE credits.track_id = t.id
                   ), ''),
                   s.provider_id, s.source_scope, s.external_id,
                   coalesce(s.path, ''), s.size_bytes, s.suffix
            FROM tracks AS t
            JOIN track_sources AS s ON s.track_id = t.id
            WHERE s.provider_id = $2
              AND t.id IN (SELECT track_id FROM duplicated)
            ORDER BY t.title_normalized, t.id, s.path
            "#,
        )
        .bind(i64::from(limit))
        .bind(LOCAL_PROVIDER_ID)
        .fetch_all(&self.pool)
        .await
        .map_err(repository_error)?;

        let mut candidates: Vec<DuplicateCandidate> = Vec::new();

        for row in rows {
            let track_id = TrackId::from_uuid(row.0);
            let source = DuplicateSource::new(
                decode_source(row.4, row.5, row.6)?,
                row.7,
                row.8.unwrap_or_default().max(0) as u64,
                row.9,
            );

            match candidates.last_mut() {
                Some(last) if last.track_id() == &track_id => last.push_source(source),
                _ => candidates.push(DuplicateCandidate::new(
                    track_id,
                    row.1,
                    row.2.and_then(|ms| u64::try_from(ms).ok()),
                    row.3
                        .split("; ")
                        .filter(|name| !name.is_empty())
                        .map(str::to_owned)
                        .collect(),
                    vec![source],
                )),
            }
        }

        Ok(candidates)
    }

    async fn split_artists(&self, limit: u32) -> Result<Vec<SplitArtist>, TrackRepositoryError> {
        let rows = sqlx::query_as::<_, (String, Uuid, String, bool, i64, i64)>(
            r#"
            WITH split AS (
                SELECT name_normalized
                FROM artists
                GROUP BY name_normalized
                HAVING count(*) > 1
                ORDER BY name_normalized
                LIMIT $1
            )
            SELECT ar.name_normalized, ar.id, ar.name,
                   ar.musicbrainz_artist_id IS NOT NULL,
                   (SELECT count(*) FROM track_artist_credits AS credits
                    WHERE credits.artist_id = ar.id),
                   (SELECT count(*) FROM albums AS releases
                    WHERE releases.album_artist_id = ar.id)
            FROM artists AS ar
            WHERE ar.name_normalized IN (SELECT name_normalized FROM split)
            ORDER BY ar.name_normalized, ar.musicbrainz_artist_id NULLS LAST
            "#,
        )
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(repository_error)?;

        let mut artists: Vec<SplitArtist> = Vec::new();

        for (normalized, artist_id, name, identified, track_count, album_count) in rows {
            let row = SplitArtistRow::new(
                ArtistId::from_uuid(artist_id),
                name,
                identified,
                track_count.max(0) as u64,
                album_count.max(0) as u64,
            );

            match artists.last_mut() {
                Some(last) if last.name() == normalized => last.push_row(row),
                _ => artists.push(SplitArtist::new(normalized, vec![row])),
            }
        }

        Ok(artists)
    }
}
