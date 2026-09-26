use async_trait::async_trait;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::features::music_library::{
    domain::{
        Album, AlbumId, AlbumMetadata, AlbumTrack, Artist, ArtistCredit, ArtistId, CreditRole,
        ProviderEntityState, ProviderId, SourceIdentity, SourceScope, Track, TrackId,
        TrackMetadata, normalize_for_matching,
    },
    storage::contracts::{ReconcileReport, SourceFileFacts, TrackRepositoryError},
};

use crate::features::music_library::storage::contracts::CatalogueWrite;

use super::PgStore;
use super::decode::{
    decode_album, decode_artist, encode_byte, encode_date, encode_date_precision,
    encode_duration_ms, encode_facts_version, encode_integer, encode_smallint, encode_source_scope,
    repository_error,
};
use super::schema::{ALBUM_COLUMNS, ARTIST_COLUMNS, AlbumRow, ArtistRow};

type ResolvedCredit = (String, ArtistId, Option<CreditRole>);

#[async_trait]
impl CatalogueWrite for PgStore {
    async fn get_or_create(
        &self,
        source: SourceIdentity,
        metadata: TrackMetadata,
    ) -> Result<Track, TrackRepositoryError> {
        let credits = resolve_credits(&self.pool, &metadata).await?;

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
        let credits = resolve_credits(&self.pool, &metadata).await?;

        let mut tx = self.pool.begin().await.map_err(repository_error)?;
        let track_id = get_or_create_in(&mut tx, &source, &metadata, &credits).await?;
        set_source_file_facts_in(&mut tx, &source, facts).await?;
        tx.commit().await.map_err(repository_error)?;

        Ok(Track::new(track_id, metadata))
    }

    async fn reconcile_missing(
        &self,
        provider: &ProviderId,
        scope: &SourceScope,
        seen: &[String],
        now: chrono::DateTime<chrono::Utc>,
    ) -> Result<ReconcileReport, TrackRepositoryError> {
        let mut tx = self.pool.begin().await.map_err(repository_error)?;

        let marked_missing = sqlx::query(
            r#"UPDATE track_sources
            SET missing_at = $3
            WHERE provider_id = $1 AND source_scope = $2
              AND path IS NOT NULL
              AND missing_at IS NULL
              AND NOT (path = ANY($4))
            "#,
        )
        .bind(provider.as_str())
        .bind(encode_source_scope(scope))
        .bind(now)
        .bind(seen)
        .execute(&mut *tx)
        .await
        .map_err(repository_error)?
        .rows_affected();

        let reappeared = sqlx::query(
            r#"UPDATE track_sources
            SET missing_at = NULL
            WHERE provider_id = $1 AND source_scope = $2
              AND path IS NOT NULL
              AND missing_at IS NOT NULL
              AND path = ANY($3)
            "#,
        )
        .bind(provider.as_str())
        .bind(encode_source_scope(scope))
        .bind(seen)
        .execute(&mut *tx)
        .await
        .map_err(repository_error)?
        .rows_affected();

        tx.commit().await.map_err(repository_error)?;

        Ok(ReconcileReport {
            marked_missing: usize::try_from(marked_missing).unwrap_or(usize::MAX),
            reappeared: usize::try_from(reappeared).unwrap_or(usize::MAX),
        })
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

    async fn set_provider_entity_state(
        &self,
        source: &SourceIdentity,
        state: &ProviderEntityState,
    ) -> Result<(), TrackRepositoryError> {
        let reported_count = state
            .reported_count()
            .map(i32::try_from)
            .transpose()
            .map_err(|_| TrackRepositoryError::InvalidData)?;

        sqlx::query(
            r#"
            INSERT INTO provider_entity_state
                (provider_id, source_scope, external_id, fetched_at, reported_count, last_error)
            VALUES ($1, $2, $3, $4, $5, $6)
            ON CONFLICT (provider_id, source_scope, external_id) DO UPDATE
            SET fetched_at = EXCLUDED.fetched_at,
                reported_count = EXCLUDED.reported_count,
                last_error = EXCLUDED.last_error
            "#,
        )
        .bind(source.provider_id().as_str())
        .bind(encode_source_scope(source.source_scope()))
        .bind(source.external_id())
        .bind(state.fetched_at())
        .bind(reported_count)
        .bind(state.last_error())
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
            INSERT INTO album_tracks
                (album_id, track_id, disc_number, track_number, position, disc_total, track_total)
            VALUES ($1, $2, $3, $4, $5, $6, $7)
            ON CONFLICT (album_id, track_id) DO UPDATE
            SET disc_number = EXCLUDED.disc_number,
                track_number = EXCLUDED.track_number,
                position = EXCLUDED.position,
                disc_total = EXCLUDED.disc_total,
                track_total = EXCLUDED.track_total
            "#,
        )
        .bind(album_id.as_uuid())
        .bind(track_id.as_uuid())
        .bind(encode_smallint(membership.disc_number())?)
        .bind(encode_smallint(membership.track_number())?)
        .bind(i32::try_from(membership.position()).map_err(|_| TrackRepositoryError::InvalidData)?)
        .bind(encode_smallint(membership.disc_total())?)
        .bind(encode_smallint(membership.track_total())?)
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
    let key = normalize_for_matching(name);

    let row = sqlx::query_as::<_, ArtistRow>(&format!(
        r#"
        SELECT {ARTIST_COLUMNS}
        FROM artists
        WHERE musicbrainz_artist_id IS NULL
          AND name_normalized = $1
        "#
    ))
    .bind(key)
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
        return apply_observed_album_facts(pool, album.id(), metadata).await;
    }

    if let Some(identifier) = metadata.musicbrainz_release_id()
        && let Some(album) = find_album_by_release(pool, identifier).await?
    {
        return apply_observed_album_facts(pool, album.id(), metadata).await;
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

    apply_observed_album_facts(pool, existing.id(), metadata).await
}

async fn apply_observed_album_facts(
    pool: &PgPool,
    album_id: &AlbumId,
    metadata: &AlbumMetadata,
) -> Result<Option<Album>, TrackRepositoryError> {
    let row = sqlx::query_as::<_, AlbumRow>(&format!(
        r#"
        UPDATE albums
        SET musicbrainz_release_group_id = COALESCE($1, musicbrainz_release_group_id),
            musicbrainz_release_id = COALESCE($2, musicbrainz_release_id),
            release_type = COALESCE($4, release_type),
            label = COALESCE($5, label),
            release_date = COALESCE($6, release_date),
            release_date_precision = COALESCE($7, release_date_precision),
            release_country = COALESCE($8, release_country),
            compilation = COALESCE($9, compilation),
            year = COALESCE(
                EXTRACT(YEAR FROM COALESCE($6, release_date))::SMALLINT,
                $3,
                year
            ),
            updated_at = CURRENT_TIMESTAMP
        WHERE id = $10
        RETURNING {ALBUM_COLUMNS}
        "#
    ))
    .bind(metadata.musicbrainz_release_group_id())
    .bind(metadata.musicbrainz_release_id())
    .bind(metadata.year())
    .bind(metadata.release_type())
    .bind(metadata.label())
    .bind(encode_date(metadata.release_date()))
    .bind(encode_date_precision(metadata.release_date()))
    .bind(metadata.release_country())
    .bind(metadata.compilation())
    .bind(album_id.as_uuid())
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
    let key = normalize_for_matching(title);

    let row = sqlx::query_as::<_, AlbumRow>(&format!(
        r#"
        SELECT {ALBUM_COLUMNS}
        FROM albums
        WHERE album_artist_id IS NOT DISTINCT FROM $1
          AND title_normalized = $2
        ORDER BY (musicbrainz_release_group_id IS NULL), created_at
        LIMIT 1
        "#
    ))
    .bind(album_artist_id.map(|id| *id.as_uuid()))
    .bind(key)
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
    let year = metadata
        .release_date()
        .map(|date| date.year())
        .or(metadata.year());
    let row = sqlx::query_as::<_, AlbumRow>(&format!(
        r#"
        INSERT INTO albums (
            id, title, album_artist_id, year, release_type, label,
            musicbrainz_release_group_id, musicbrainz_release_id,
            release_date, release_date_precision, release_country, compilation
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12)
        ON CONFLICT DO NOTHING
        RETURNING {ALBUM_COLUMNS}
        "#
    ))
    .bind(AlbumId::new().as_uuid())
    .bind(metadata.title())
    .bind(album_artist_id.map(|id| *id.as_uuid()))
    .bind(year)
    .bind(metadata.release_type())
    .bind(metadata.label())
    .bind(metadata.musicbrainz_release_group_id())
    .bind(metadata.musicbrainz_release_id())
    .bind(encode_date(metadata.release_date()))
    .bind(encode_date_precision(metadata.release_date()))
    .bind(metadata.release_country())
    .bind(metadata.compilation())
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
            content_type = $5, suffix = $6, acoustid_fingerprint = $7,
            container = $8, codec = $9, bitrate_kbps = $10, sample_rate_hz = $11,
            bit_depth = $12, channels = $13, facts_version = $14
        WHERE provider_id = $15 AND source_scope = $16 AND external_id = $17
        "#,
    )
    .bind(facts.path.as_deref())
    .bind(facts.size_bytes as i64)
    .bind(facts.modified_at)
    .bind(facts.missing_at)
    .bind(facts.content_type.as_deref())
    .bind(facts.suffix.as_deref())
    .bind(facts.acoustid_fingerprint.as_deref())
    .bind(facts.container.as_deref())
    .bind(facts.codec.as_deref())
    .bind(encode_integer(facts.bitrate_kbps)?)
    .bind(encode_integer(facts.sample_rate_hz)?)
    .bind(encode_byte(facts.bit_depth))
    .bind(encode_byte(facts.channels))
    .bind(encode_facts_version(facts.facts_version)?)
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
    credits: &[ResolvedCredit],
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
    replace_track_genres(connection, &track_id, metadata.genres()).await?;

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
        INSERT INTO tracks (
            id, title, duration_ms, musicbrainz_recording_id, isrc, genre,
            recording_date, recording_date_precision, original_date, original_date_precision
        )
        VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
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
    .bind(metadata.primary_genre())
    .bind(encode_date(metadata.recording_date()))
    .bind(encode_date_precision(metadata.recording_date()))
    .bind(encode_date(metadata.original_date()))
    .bind(encode_date_precision(metadata.original_date()))
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
            genre = $5,
            recording_date = $6,
            recording_date_precision = $7,
            original_date = $8,
            original_date_precision = $9,
            updated_at = CURRENT_TIMESTAMP
        WHERE id = $10
        "#,
    )
    .bind(metadata.title())
    .bind(encode_duration_ms(metadata.duration_ms())?)
    .bind(metadata.musicbrainz_recording_id())
    .bind(metadata.isrc())
    .bind(metadata.primary_genre())
    .bind(encode_date(metadata.recording_date()))
    .bind(encode_date_precision(metadata.recording_date()))
    .bind(encode_date(metadata.original_date()))
    .bind(encode_date_precision(metadata.original_date()))
    .bind(track_id.as_uuid())
    .execute(&mut *connection)
    .await
    .map_err(repository_error)?;

    if result.rows_affected() != 1 {
        return Err(TrackRepositoryError::InvalidData);
    }

    Ok(())
}

async fn replace_track_genres(
    connection: &mut PgConnection,
    track_id: &TrackId,
    genres: &[String],
) -> Result<(), TrackRepositoryError> {
    sqlx::query("DELETE FROM track_genres WHERE track_id = $1")
        .bind(track_id.as_uuid())
        .execute(&mut *connection)
        .await
        .map_err(repository_error)?;

    for (position, name) in genres.iter().enumerate() {
        let position = i32::try_from(position).map_err(|_| TrackRepositoryError::InvalidData)?;

        sqlx::query(
            r#"
            INSERT INTO track_genres (track_id, position, name)
            VALUES ($1, $2, $3)
            "#,
        )
        .bind(track_id.as_uuid())
        .bind(position)
        .bind(name)
        .execute(&mut *connection)
        .await
        .map_err(repository_error)?;
    }

    Ok(())
}

async fn replace_artist_credits(
    connection: &mut PgConnection,
    track_id: &TrackId,
    credits: &[ResolvedCredit],
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

    for (position, (name, artist_id, role)) in credits.iter().enumerate() {
        let position = i32::try_from(position).map_err(|_| TrackRepositoryError::InvalidData)?;

        sqlx::query(
            r#"
            INSERT INTO track_artist_credits (track_id, position, name, artist_id, role)
            VALUES ($1, $2, $3, $4, $5)
            "#,
        )
        .bind(track_id.as_uuid())
        .bind(position)
        .bind(name)
        .bind(artist_id.as_uuid())
        .bind(role.map(|role| role.as_str()))
        .execute(&mut *connection)
        .await
        .map_err(repository_error)?;
    }

    Ok(())
}

async fn resolve_credits(
    pool: &PgPool,
    metadata: &TrackMetadata,
) -> Result<Vec<ResolvedCredit>, TrackRepositoryError> {
    let credits = metadata.artists().iter().chain(metadata.credits());
    let mut resolved = Vec::with_capacity(metadata.artists().len() + metadata.credits().len());

    for credit in credits {
        let artist = resolve_artist(pool, credit).await?;
        resolved.push((credit.name().to_owned(), artist.id().clone(), credit.role()));
    }

    Ok(resolved)
}
