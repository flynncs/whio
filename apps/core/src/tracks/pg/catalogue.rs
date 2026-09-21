use async_trait::async_trait;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::tracks::{
    Album, AlbumId, AlbumMetadata, AlbumTrack, Artist, ArtistCredit, ArtistId, SourceFileFacts,
    SourceIdentity, Track, TrackId, TrackMetadata, TrackRepositoryError,
};

use crate::tracks::CatalogueWrite;

use super::PgStore;
use super::decode::{
    decode_album, decode_artist, encode_duration_ms, encode_smallint, encode_source_scope,
    repository_error,
};
use super::schema::{ALBUM_COLUMNS, ARTIST_COLUMNS, AlbumRow, ArtistRow};

#[async_trait]
impl CatalogueWrite for PgStore {
    async fn get_or_create(
        &self,
        source: SourceIdentity,
        metadata: TrackMetadata,
    ) -> Result<Track, TrackRepositoryError> {
        let credits = resolve_credits(&self.pool, metadata.artists()).await?;

        let mut tx = self.pool.begin().await.map_err(repository_error)?;
        let track_id = get_or_create_in(&mut tx, &source, &metadata, &credits).await?;
        tx.commit().await.map_err(repository_error)?;

        Ok(Track::new(track_id, metadata))
    }

    async fn index_source(
        &self,
        source: SourceIdentity,
        metadata: TrackMetadata,
        facts: &SourceFileFacts,
    ) -> Result<Track, TrackRepositoryError> {
        let credits = resolve_credits(&self.pool, metadata.artists()).await?;

        let mut tx = self.pool.begin().await.map_err(repository_error)?;
        let track_id = get_or_create_in(&mut tx, &source, &metadata, &credits).await?;
        set_source_file_facts_in(&mut tx, &source, facts).await?;
        tx.commit().await.map_err(repository_error)?;

        Ok(Track::new(track_id, metadata))
    }

    async fn resolve_or_create_artist(
        &self,
        credit: &ArtistCredit,
    ) -> Result<Artist, TrackRepositoryError> {
        resolve_artist(&self.pool, credit).await
    }

    async fn resolve_or_create_album(
        &self,
        metadata: &AlbumMetadata,
    ) -> Result<Album, TrackRepositoryError> {
        resolve_album(&self.pool, metadata).await
    }

    async fn set_album_source(
        &self,
        album_id: &AlbumId,
        source: &SourceIdentity,
    ) -> Result<(), TrackRepositoryError> {
        sqlx::query(
            r#"
            INSERT INTO album_sources (id, album_id, provider_id, source_scope, external_id)
            VALUES ($1, $2, $3, $4, $5)
            ON CONFLICT (provider_id, source_scope, external_id) DO NOTHING
            "#,
        )
        .bind(Uuid::now_v7())
        .bind(album_id.as_uuid())
        .bind(source.provider_id().as_str())
        .bind(encode_source_scope(source.source_scope()))
        .bind(source.external_id())
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(repository_error)
    }

    async fn set_artist_source(
        &self,
        artist_id: &ArtistId,
        source: &SourceIdentity,
    ) -> Result<(), TrackRepositoryError> {
        sqlx::query(
            r#"
            INSERT INTO artist_sources (id, artist_id, provider_id, source_scope, external_id)
            VALUES ($1, $2, $3, $4, $5)
            ON CONFLICT (provider_id, source_scope, external_id) DO NOTHING
            "#,
        )
        .bind(Uuid::now_v7())
        .bind(artist_id.as_uuid())
        .bind(source.provider_id().as_str())
        .bind(encode_source_scope(source.source_scope()))
        .bind(source.external_id())
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(repository_error)
    }

    async fn set_album_track(
        &self,
        album_id: &AlbumId,
        track_id: &TrackId,
        membership: &AlbumTrack,
    ) -> Result<(), TrackRepositoryError> {
        let mut tx = self.pool.begin().await.map_err(repository_error)?;

        sqlx::query("DELETE FROM album_tracks WHERE track_id = $1 AND album_id <> $2")
            .bind(track_id.as_uuid())
            .bind(album_id.as_uuid())
            .execute(&mut *tx)
            .await
            .map_err(repository_error)?;

        sqlx::query(
            r#"
            INSERT INTO album_tracks (album_id, track_id, disc_number, track_number, position)
            VALUES ($1, $2, $3, $4, $5)
            ON CONFLICT (album_id, track_id) DO UPDATE
            SET disc_number = EXCLUDED.disc_number,
                track_number = EXCLUDED.track_number,
                position = EXCLUDED.position
            "#,
        )
        .bind(album_id.as_uuid())
        .bind(track_id.as_uuid())
        .bind(encode_smallint(membership.disc_number())?)
        .bind(encode_smallint(membership.track_number())?)
        .bind(i32::try_from(membership.position()).map_err(|_| TrackRepositoryError::InvalidData)?)
        .execute(&mut *tx)
        .await
        .map_err(repository_error)?;

        tx.commit().await.map_err(repository_error)?;

        Ok(())
    }

    async fn clear_album_track(&self, track_id: &TrackId) -> Result<(), TrackRepositoryError> {
        sqlx::query("DELETE FROM album_tracks WHERE track_id = $1")
            .bind(track_id.as_uuid())
            .execute(&self.pool)
            .await
            .map(|_| ())
            .map_err(repository_error)
    }

    async fn set_source_file_facts(
        &self,
        source: &SourceIdentity,
        facts: &SourceFileFacts,
    ) -> Result<(), TrackRepositoryError> {
        let mut connection = self.pool.acquire().await.map_err(repository_error)?;

        set_source_file_facts_in(&mut connection, source, facts).await
    }
}

async fn resolve_artist(
    pool: &PgPool,
    credit: &ArtistCredit,
) -> Result<Artist, TrackRepositoryError> {
    for _ in 0..2 {
        if let Some(artist) = find_or_attach_artist(pool, credit).await? {
            return Ok(artist);
        }
        if let Some(artist) = insert_artist(pool, credit).await? {
            return Ok(artist);
        }
    }

    Err(TrackRepositoryError::InvalidData)
}

async fn find_or_attach_artist(
    pool: &PgPool,
    credit: &ArtistCredit,
) -> Result<Option<Artist>, TrackRepositoryError> {
    if let Some(identifier) = credit.musicbrainz_artist_id()
        && let Some(artist) = find_artist_by_identifier(pool, identifier).await?
    {
        return Ok(Some(artist));
    }

    let Some(existing) = find_artist_by_name(pool, credit.name()).await? else {
        return Ok(None);
    };

    let Some(identifier) = credit.musicbrainz_artist_id() else {
        return Ok(Some(existing));
    };

    let row = sqlx::query_as::<_, ArtistRow>(&format!(
        r#"
        UPDATE artists
        SET musicbrainz_artist_id = $1, updated_at = CURRENT_TIMESTAMP
        WHERE id = $2 AND musicbrainz_artist_id IS NULL
        RETURNING {ARTIST_COLUMNS}
        "#
    ))
    .bind(identifier)
    .bind(existing.id().as_uuid())
    .fetch_optional(pool)
    .await
    .map_err(repository_error)?;

    match row {
        Some(row) => Ok(Some(decode_artist(row)?)),
        None => find_artist_by_identifier(pool, identifier).await,
    }
}

async fn find_artist_by_identifier(
    pool: &PgPool,
    identifier: &str,
) -> Result<Option<Artist>, TrackRepositoryError> {
    let row = sqlx::query_as::<_, ArtistRow>(&format!(
        "SELECT {ARTIST_COLUMNS} FROM artists WHERE musicbrainz_artist_id = $1"
    ))
    .bind(identifier)
    .fetch_optional(pool)
    .await
    .map_err(repository_error)?;

    row.map(decode_artist).transpose()
}

async fn find_artist_by_name(
    pool: &PgPool,
    name: &str,
) -> Result<Option<Artist>, TrackRepositoryError> {
    let row = sqlx::query_as::<_, ArtistRow>(&format!(
        r#"
        SELECT {ARTIST_COLUMNS}
        FROM artists
        WHERE musicbrainz_artist_id IS NULL
          AND name_normalized = lower(regexp_replace(normalize($1, NFC), '\s+', ' ', 'g'))
        "#
    ))
    .bind(name)
    .fetch_optional(pool)
    .await
    .map_err(repository_error)?;

    row.map(decode_artist).transpose()
}

async fn insert_artist(
    pool: &PgPool,
    credit: &ArtistCredit,
) -> Result<Option<Artist>, TrackRepositoryError> {
    let row = sqlx::query_as::<_, ArtistRow>(&format!(
        r#"
        INSERT INTO artists (id, name, musicbrainz_artist_id)
        VALUES ($1, $2, $3)
        ON CONFLICT DO NOTHING
        RETURNING {ARTIST_COLUMNS}
        "#
    ))
    .bind(ArtistId::new().as_uuid())
    .bind(credit.name())
    .bind(credit.musicbrainz_artist_id())
    .fetch_optional(pool)
    .await
    .map_err(repository_error)?;

    row.map(decode_artist).transpose()
}

async fn resolve_album(
    pool: &PgPool,
    metadata: &AlbumMetadata,
) -> Result<Album, TrackRepositoryError> {
    let album_artist_id = match metadata.album_artist() {
        Some(credit) => Some(resolve_artist(pool, credit).await?.id().clone()),
        None => None,
    };

    for _ in 0..2 {
        if let Some(album) = find_or_attach_album(pool, metadata, album_artist_id.as_ref()).await? {
            return Ok(album);
        }
        if let Some(album) = insert_album(pool, metadata, album_artist_id.as_ref()).await? {
            return Ok(album);
        }
    }

    Err(TrackRepositoryError::InvalidData)
}

async fn find_or_attach_album(
    pool: &PgPool,
    metadata: &AlbumMetadata,
    album_artist_id: Option<&ArtistId>,
) -> Result<Option<Album>, TrackRepositoryError> {
    if let Some(identifier) = metadata.musicbrainz_release_group_id()
        && let Some(album) = find_album_by_release_group(pool, identifier).await?
    {
        return Ok(Some(album));
    }

    if let Some(identifier) = metadata.musicbrainz_release_id()
        && let Some(album) = find_album_by_release(pool, identifier).await?
    {
        return Ok(Some(album));
    }

    let Some(existing) = find_album_by_text(pool, metadata.title(), album_artist_id).await? else {
        return Ok(None);
    };

    let existing_has_identifier = existing.musicbrainz_release_group_id().is_some()
        || existing.musicbrainz_release_id().is_some();
    let incoming_has_identifier = metadata.musicbrainz_release_group_id().is_some()
        || metadata.musicbrainz_release_id().is_some();

    if existing_has_identifier && incoming_has_identifier {
        return Ok(None);
    }
    if !incoming_has_identifier {
        return Ok(Some(existing));
    }

    let row = sqlx::query_as::<_, AlbumRow>(&format!(
        r#"
        UPDATE albums
        SET musicbrainz_release_group_id = COALESCE($1, musicbrainz_release_group_id),
            musicbrainz_release_id = COALESCE($2, musicbrainz_release_id),
            year = COALESCE(year, $3),
            release_type = COALESCE(release_type, $4),
            label = COALESCE(label, $5),
            updated_at = CURRENT_TIMESTAMP
        WHERE id = $6
        RETURNING {ALBUM_COLUMNS}
        "#
    ))
    .bind(metadata.musicbrainz_release_group_id())
    .bind(metadata.musicbrainz_release_id())
    .bind(metadata.year())
    .bind(metadata.release_type())
    .bind(metadata.label())
    .bind(existing.id().as_uuid())
    .fetch_optional(pool)
    .await
    .map_err(repository_error)?;

    match row {
        Some(row) => Ok(Some(decode_album(row)?)),
        None => Ok(None),
    }
}

async fn find_album_by_release_group(
    pool: &PgPool,
    identifier: &str,
) -> Result<Option<Album>, TrackRepositoryError> {
    let row = sqlx::query_as::<_, AlbumRow>(&format!(
        "SELECT {ALBUM_COLUMNS} FROM albums WHERE musicbrainz_release_group_id = $1"
    ))
    .bind(identifier)
    .fetch_optional(pool)
    .await
    .map_err(repository_error)?;

    row.map(decode_album).transpose()
}

async fn find_album_by_release(
    pool: &PgPool,
    identifier: &str,
) -> Result<Option<Album>, TrackRepositoryError> {
    let row = sqlx::query_as::<_, AlbumRow>(&format!(
        "SELECT {ALBUM_COLUMNS} FROM albums WHERE musicbrainz_release_id = $1"
    ))
    .bind(identifier)
    .fetch_optional(pool)
    .await
    .map_err(repository_error)?;

    row.map(decode_album).transpose()
}

async fn find_album_by_text(
    pool: &PgPool,
    title: &str,
    album_artist_id: Option<&ArtistId>,
) -> Result<Option<Album>, TrackRepositoryError> {
    let row = sqlx::query_as::<_, AlbumRow>(&format!(
        r#"
        SELECT {ALBUM_COLUMNS}
        FROM albums
        WHERE album_artist_id IS NOT DISTINCT FROM $1
          AND title_normalized = lower(regexp_replace(normalize($2, NFC), '\s+', ' ', 'g'))
        ORDER BY (musicbrainz_release_group_id IS NULL), created_at
        LIMIT 1
        "#
    ))
    .bind(album_artist_id.map(|id| *id.as_uuid()))
    .bind(title)
    .fetch_optional(pool)
    .await
    .map_err(repository_error)?;

    row.map(decode_album).transpose()
}

async fn insert_album(
    pool: &PgPool,
    metadata: &AlbumMetadata,
    album_artist_id: Option<&ArtistId>,
) -> Result<Option<Album>, TrackRepositoryError> {
    let row = sqlx::query_as::<_, AlbumRow>(&format!(
        r#"
        INSERT INTO albums (
            id, title, album_artist_id, year, release_type, label,
            musicbrainz_release_group_id, musicbrainz_release_id
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
        ON CONFLICT DO NOTHING
        RETURNING {ALBUM_COLUMNS}
        "#
    ))
    .bind(AlbumId::new().as_uuid())
    .bind(metadata.title())
    .bind(album_artist_id.map(|id| *id.as_uuid()))
    .bind(metadata.year())
    .bind(metadata.release_type())
    .bind(metadata.label())
    .bind(metadata.musicbrainz_release_group_id())
    .bind(metadata.musicbrainz_release_id())
    .fetch_optional(pool)
    .await
    .map_err(repository_error)?;

    row.map(decode_album).transpose()
}

async fn set_source_file_facts_in(
    connection: &mut PgConnection,
    source: &SourceIdentity,
    facts: &SourceFileFacts,
) -> Result<(), TrackRepositoryError> {
    sqlx::query(
        r#"UPDATE track_sources
        SET path = $1, size_bytes = $2, modified_at = $3, missing_at = $4,
            content_type = $5, suffix = $6, acoustid_fingerprint = $7
        WHERE provider_id = $8 AND source_scope = $9 AND external_id = $10
        "#,
    )
    .bind(facts.path.as_deref())
    .bind(facts.size_bytes as i64)
    .bind(facts.modified_at)
    .bind(facts.missing_at)
    .bind(facts.content_type.as_deref())
    .bind(facts.suffix.as_deref())
    .bind(facts.acoustid_fingerprint.as_deref())
    .bind(source.provider_id().as_str())
    .bind(encode_source_scope(source.source_scope()))
    .bind(source.external_id())
    .execute(&mut *connection)
    .await
    .map(|_| ())
    .map_err(repository_error)
}

async fn get_or_create_in(
    connection: &mut PgConnection,
    source: &SourceIdentity,
    metadata: &TrackMetadata,
    credits: &[(String, ArtistId)],
) -> Result<TrackId, TrackRepositoryError> {
    let track_id = match resolve_identified_track(connection, metadata).await? {
        Some(identified) => {
            let previous = find_track_id_by_source_for_update(connection, source).await?;
            attach_source(connection, &identified, source).await?;

            if let Some(previous) = previous
                && previous != identified
            {
                delete_track_if_source_less(connection, &previous).await?;
            }

            identified
        }
        None => match find_track_id_by_source_for_update(connection, source).await? {
            Some(current) => current,
            None => create_with_source(connection, source, metadata).await?,
        },
    };

    update_track_metadata(connection, &track_id, metadata).await?;
    replace_artist_credits(connection, &track_id, credits).await?;

    Ok(track_id)
}

async fn resolve_identified_track(
    connection: &mut PgConnection,
    metadata: &TrackMetadata,
) -> Result<Option<TrackId>, TrackRepositoryError> {
    if let Some(track_id) =
        find_track_id_by_recording_id(connection, metadata.musicbrainz_recording_id()).await?
    {
        return Ok(Some(track_id));
    }

    find_track_id_by_isrc(connection, metadata.isrc()).await
}

async fn find_track_id_by_recording_id(
    connection: &mut PgConnection,
    recording_id: Option<&str>,
) -> Result<Option<TrackId>, TrackRepositoryError> {
    let Some(recording_id) = recording_id else {
        return Ok(None);
    };

    sqlx::query_scalar::<_, Uuid>("SELECT id FROM tracks WHERE musicbrainz_recording_id = $1")
        .bind(recording_id)
        .fetch_optional(&mut *connection)
        .await
        .map(|row| row.map(TrackId::from_uuid))
        .map_err(repository_error)
}

async fn find_track_id_by_isrc(
    connection: &mut PgConnection,
    isrc: Option<&str>,
) -> Result<Option<TrackId>, TrackRepositoryError> {
    let Some(isrc) = isrc else {
        return Ok(None);
    };

    let rows =
        sqlx::query_scalar::<_, Uuid>("SELECT id FROM tracks WHERE isrc = $1 ORDER BY id LIMIT 2")
            .bind(isrc)
            .fetch_all(&mut *connection)
            .await
            .map_err(repository_error)?;

    Ok(match rows.as_slice() {
        [only] => Some(TrackId::from_uuid(*only)),
        _ => None,
    })
}

async fn create_with_source(
    connection: &mut PgConnection,
    source: &SourceIdentity,
    metadata: &TrackMetadata,
) -> Result<TrackId, TrackRepositoryError> {
    let candidate = TrackId::new();

    if !insert_candidate_track(connection, &candidate, metadata).await? {
        let winner = find_track_id_by_recording_id(connection, metadata.musicbrainz_recording_id())
            .await?
            .ok_or(TrackRepositoryError::Unavailable)?;

        attach_source(connection, &winner, source).await?;

        return Ok(winner);
    }

    if try_insert_source(connection, &candidate, source).await? {
        return Ok(candidate);
    }

    resolve_concurrent_winner(connection, &candidate, source).await
}

async fn attach_source(
    connection: &mut PgConnection,
    track_id: &TrackId,
    source: &SourceIdentity,
) -> Result<(), TrackRepositoryError> {
    sqlx::query(
        r#"
        INSERT INTO track_sources (
            id,
            track_id,
            provider_id,
            source_scope,
            external_id
        )
        VALUES ($1, $2, $3, $4, $5)
        ON CONFLICT (provider_id, source_scope, external_id)
        DO UPDATE SET track_id = EXCLUDED.track_id
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(track_id.as_uuid())
    .bind(source.provider_id().as_str())
    .bind(encode_source_scope(source.source_scope()))
    .bind(source.external_id())
    .execute(&mut *connection)
    .await
    .map(|_| ())
    .map_err(repository_error)
}

async fn delete_track_if_source_less(
    connection: &mut PgConnection,
    track_id: &TrackId,
) -> Result<(), TrackRepositoryError> {
    sqlx::query(
        r#"
        DELETE FROM tracks
        WHERE id = $1
          AND NOT EXISTS (
              SELECT 1 FROM track_sources WHERE track_sources.track_id = tracks.id
          )
        "#,
    )
    .bind(track_id.as_uuid())
    .execute(&mut *connection)
    .await
    .map(|_| ())
    .map_err(repository_error)
}

async fn find_track_id_by_source_for_update(
    connection: &mut PgConnection,
    source: &SourceIdentity,
) -> Result<Option<TrackId>, TrackRepositoryError> {
    sqlx::query_scalar::<_, Uuid>(
        r#"
        SELECT track_id
        FROM track_sources
        WHERE provider_id = $1
          AND source_scope = $2
          AND external_id = $3
        FOR UPDATE
        "#,
    )
    .bind(source.provider_id().as_str())
    .bind(encode_source_scope(source.source_scope()))
    .bind(source.external_id())
    .fetch_optional(&mut *connection)
    .await
    .map(|track_id| track_id.map(TrackId::from_uuid))
    .map_err(repository_error)
}

async fn insert_candidate_track(
    connection: &mut PgConnection,
    track_id: &TrackId,
    metadata: &TrackMetadata,
) -> Result<bool, TrackRepositoryError> {
    sqlx::query_scalar::<_, Uuid>(
        r#"
        INSERT INTO tracks (id, title, duration_ms, musicbrainz_recording_id, isrc, genre)
        VALUES ($1, $2, $3, $4, $5, $6)
        ON CONFLICT (musicbrainz_recording_id) WHERE musicbrainz_recording_id IS NOT NULL
        DO NOTHING
        RETURNING id
        "#,
    )
    .bind(track_id.as_uuid())
    .bind(metadata.title())
    .bind(encode_duration_ms(metadata.duration_ms())?)
    .bind(metadata.musicbrainz_recording_id())
    .bind(metadata.isrc())
    .bind(metadata.genre())
    .fetch_optional(&mut *connection)
    .await
    .map(|row| row.is_some())
    .map_err(repository_error)
}

async fn try_insert_source(
    connection: &mut PgConnection,
    track_id: &TrackId,
    source: &SourceIdentity,
) -> Result<bool, TrackRepositoryError> {
    sqlx::query_scalar::<_, Uuid>(
        r#"
        INSERT INTO track_sources (
            id,
            track_id,
            provider_id,
            source_scope,
            external_id
        )
        VALUES ($1, $2, $3, $4, $5)
        ON CONFLICT (provider_id, source_scope, external_id)
        DO NOTHING
        RETURNING track_id
        "#,
    )
    .bind(Uuid::now_v7())
    .bind(track_id.as_uuid())
    .bind(source.provider_id().as_str())
    .bind(encode_source_scope(source.source_scope()))
    .bind(source.external_id())
    .fetch_optional(&mut *connection)
    .await
    .map(|track_id| track_id.is_some())
    .map_err(repository_error)
}

async fn resolve_concurrent_winner(
    connection: &mut PgConnection,
    candidate_track_id: &TrackId,
    source: &SourceIdentity,
) -> Result<TrackId, TrackRepositoryError> {
    let result = sqlx::query(
        r#"
        DELETE FROM tracks
        WHERE id = $1
        "#,
    )
    .bind(candidate_track_id.as_uuid())
    .execute(&mut *connection)
    .await
    .map_err(repository_error)?;

    if result.rows_affected() != 1 {
        return Err(TrackRepositoryError::InvalidData);
    }

    find_track_id_by_source_for_update(connection, source)
        .await?
        .ok_or(TrackRepositoryError::InvalidData)
}

async fn update_track_metadata(
    connection: &mut PgConnection,
    track_id: &TrackId,
    metadata: &TrackMetadata,
) -> Result<(), TrackRepositoryError> {
    let result = sqlx::query(
        r#"
        UPDATE tracks
        SET title = $1,
            duration_ms = $2,
            musicbrainz_recording_id = COALESCE($3, musicbrainz_recording_id),
            isrc = COALESCE($4, isrc),
            genre = COALESCE($5, genre),
            updated_at = CURRENT_TIMESTAMP
        WHERE id = $6
        "#,
    )
    .bind(metadata.title())
    .bind(encode_duration_ms(metadata.duration_ms())?)
    .bind(metadata.musicbrainz_recording_id())
    .bind(metadata.isrc())
    .bind(metadata.genre())
    .bind(track_id.as_uuid())
    .execute(&mut *connection)
    .await
    .map_err(repository_error)?;

    if result.rows_affected() != 1 {
        return Err(TrackRepositoryError::InvalidData);
    }

    Ok(())
}

async fn replace_artist_credits(
    connection: &mut PgConnection,
    track_id: &TrackId,
    credits: &[(String, ArtistId)],
) -> Result<(), TrackRepositoryError> {
    sqlx::query(
        r#"
        DELETE FROM track_artist_credits
        WHERE track_id = $1
        "#,
    )
    .bind(track_id.as_uuid())
    .execute(&mut *connection)
    .await
    .map_err(repository_error)?;

    for (position, (name, artist_id)) in credits.iter().enumerate() {
        let position = i32::try_from(position).map_err(|_| TrackRepositoryError::InvalidData)?;

        sqlx::query(
            r#"
            INSERT INTO track_artist_credits (track_id, position, name, artist_id)
            VALUES ($1, $2, $3, $4)
            "#,
        )
        .bind(track_id.as_uuid())
        .bind(position)
        .bind(name)
        .bind(artist_id.as_uuid())
        .execute(&mut *connection)
        .await
        .map_err(repository_error)?;
    }

    Ok(())
}

async fn resolve_credits(
    pool: &PgPool,
    credits: &[ArtistCredit],
) -> Result<Vec<(String, ArtistId)>, TrackRepositoryError> {
    let mut resolved = Vec::with_capacity(credits.len());

    for credit in credits {
        let artist = resolve_artist(pool, credit).await?;
        resolved.push((credit.name().to_owned(), artist.id().clone()));
    }

    Ok(resolved)
}
