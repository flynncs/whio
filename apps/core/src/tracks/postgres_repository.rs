use async_trait::async_trait;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::tracks::{
    ArtworkRef, ProviderId, SourceIdentity, SourceScope, Track, TrackId, TrackMetadata,
    TrackRepository, TrackRepositoryError,
};

pub struct PostgresTrackRepository {
    pool: PgPool,
}

impl PostgresTrackRepository {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }
}

#[async_trait]
impl TrackRepository for PostgresTrackRepository {
    async fn get_or_create(
        &self,
        source: SourceIdentity,
        metadata: TrackMetadata,
    ) -> Result<Track, TrackRepositoryError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        let track_id = find_track_id_by_source_for_update(&mut tx, &source).await?;

        let track_id = if let Some(track_id) = track_id {
            track_id
        } else {
            let candidate_track_id = TrackId::new();

            insert_candidate_track(&mut tx, &candidate_track_id, &source, &metadata).await?;

            if try_insert_source(&mut tx, &candidate_track_id, &source).await? {
                candidate_track_id
            } else {
                resolve_concurrent_winner(&mut tx, &candidate_track_id, &source).await?
            }
        };

        let album = update_track_metadata(&mut tx, &track_id, &source, &metadata).await?;
        replace_artist_credits(&mut tx, &track_id, &metadata).await?;

        tx.commit()
            .await
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        let metadata = metadata
            .with_album(album)
            .map_err(|_| TrackRepositoryError::InvalidData)?;

        Ok(Track::new(track_id, metadata))
    }

    async fn find_sources(
        &self,
        track_id: &TrackId,
    ) -> Result<Vec<SourceIdentity>, TrackRepositoryError> {
        let rows = sqlx::query_as::<_, (String, String, String)>(
            r#"SELECT provider_id, source_scope, external_id
            FROM track_sources
            WHERE track_id = $1
            ORDER BY created_at, id
            "#,
        )
        .bind(track_id.as_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(|_| TrackRepositoryError::Unavailable)?;

        rows.into_iter()
            .map(|(provider_id, source_scope, external_id)| {
                decode_source(provider_id, source_scope, external_id)
            })
            .collect()
    }

    async fn find_track(&self, track_id: &TrackId) -> Result<Option<Track>, TrackRepositoryError> {
        let row: Option<(String, Option<i64>, Option<String>)> = sqlx::query_as(
            r#"SELECT title, duration_ms, album
            FROM tracks
            WHERE id = $1
            "#,
        )
        .bind(track_id.as_uuid())
        .fetch_optional(&self.pool)
        .await
        .map_err(|_| TrackRepositoryError::Unavailable)?;

        let Some((title, duration_ms, album)) = row else {
            return Ok(None);
        };

        let artists: Vec<String> = sqlx::query_scalar(
            r#"SELECT name
            FROM track_artist_credits
            WHERE track_id = $1
            ORDER BY position
            "#,
        )
        .bind(track_id.as_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(|_| TrackRepositoryError::Unavailable)?;

        let duration_ms = duration_ms
            .map(u64::try_from)
            .transpose()
            .map_err(|_| TrackRepositoryError::InvalidData)?;
        let metadata = TrackMetadata::new(title, artists, duration_ms)
            .map_err(|_| TrackRepositoryError::InvalidData)?
            .with_album(album)
            .map_err(|_| TrackRepositoryError::InvalidData)?;

        Ok(Some(Track::new(
            TrackId::from_uuid(*track_id.as_uuid()),
            metadata,
        )))
    }

    async fn set_source_artwork(
        &self,
        source: &SourceIdentity,
        artwork: &ArtworkRef,
    ) -> Result<(), TrackRepositoryError> {
        sqlx::query(
            r#"UPDATE track_sources
            SET artwork_kind = $1, artwork_ref = $2
            WHERE provider_id = $3 AND source_scope = $4 AND external_id = $5
            "#,
        )
        .bind(artwork.kind())
        .bind(artwork.reference())
        .bind(source.provider_id().as_str())
        .bind(encode_source_scope(source.source_scope()))
        .bind(source.external_id())
        .execute(&self.pool)
        .await
        .map(|_| ())
        .map_err(|_| TrackRepositoryError::Unavailable)
    }

    async fn find_artwork(
        &self,
        track_id: &TrackId,
    ) -> Result<Option<(SourceIdentity, ArtworkRef)>, TrackRepositoryError> {
        let rows = sqlx::query_as::<_, (String, String, String, String, String)>(
            r#"SELECT provider_id, source_scope, external_id, artwork_kind, artwork_ref
            FROM track_sources
            WHERE track_id = $1
              AND artwork_kind IS NOT NULL
              AND artwork_ref IS NOT NULL
            ORDER BY created_at, id
            "#,
        )
        .bind(track_id.as_uuid())
        .fetch_all(&self.pool)
        .await
        .map_err(|_| TrackRepositoryError::Unavailable)?;

        let mut best: Option<(SourceIdentity, ArtworkRef)> = None;
        for (provider_id, source_scope, external_id, kind, reference) in rows {
            let source = decode_source(provider_id, source_scope, external_id)?;
            let artwork = ArtworkRef::from_stored(&kind, reference)
                .map_err(|_| TrackRepositoryError::InvalidData)?;

            let outranks = best.as_ref().is_none_or(|(current, _)| {
                source.provider_id().precedence() < current.provider_id().precedence()
            });

            if outranks {
                best = Some((source, artwork));
            }
        }

        Ok(best)
    }
}

fn decode_source(
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
    .map_err(|_| TrackRepositoryError::Unavailable)
}

async fn insert_candidate_track(
    connection: &mut PgConnection,
    track_id: &TrackId,
    source: &SourceIdentity,
    metadata: &TrackMetadata,
) -> Result<(), TrackRepositoryError> {
    sqlx::query(
        r#"
        INSERT INTO tracks (id, title, duration_ms, album, album_source)
        VALUES ($1, $2, $3, $4, $5)
        "#,
    )
    .bind(track_id.as_uuid())
    .bind(metadata.title())
    .bind(encode_duration_ms(metadata.duration_ms())?)
    .bind(metadata.album())
    .bind(metadata.album().map(|_| source.provider_id().as_str()))
    .execute(&mut *connection)
    .await
    .map(|_| ())
    .map_err(|_| TrackRepositoryError::Unavailable)
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
    .map_err(|_| TrackRepositoryError::Unavailable)
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
    .map_err(|_| TrackRepositoryError::Unavailable)?;

    if result.rows_affected() != 1 {
        return Err(TrackRepositoryError::InvalidData);
    }

    find_track_id_by_source_for_update(connection, source)
        .await?
        .ok_or(TrackRepositoryError::InvalidData)
}

/// merges display metadata and returns the album that won provider precedence.
async fn update_track_metadata(
    connection: &mut PgConnection,
    track_id: &TrackId,
    source: &SourceIdentity,
    metadata: &TrackMetadata,
) -> Result<Option<String>, TrackRepositoryError> {
    let (existing_album, existing_source): (Option<String>, Option<String>) = sqlx::query_as(
        r#"
        SELECT album, album_source
        FROM tracks
        WHERE id = $1
        FOR UPDATE
        "#,
    )
    .bind(track_id.as_uuid())
    .fetch_one(&mut *connection)
    .await
    .map_err(|_| TrackRepositoryError::Unavailable)?;

    let (album, album_source) = match metadata.album() {
        None => (existing_album, existing_source),
        Some(incoming) => {
            let existing_rank = existing_source
                .as_deref()
                .and_then(|value| ProviderId::new(value.to_owned()).ok())
                .map_or(u8::MAX, |provider| provider.precedence());

            // a higher-precedence provider replaces the album; within one
            // provider the fresher observation wins.
            if existing_album.is_some() && existing_rank < source.provider_id().precedence() {
                (existing_album, existing_source)
            } else {
                (
                    Some(incoming.to_owned()),
                    Some(source.provider_id().as_str().to_owned()),
                )
            }
        }
    };

    let result = sqlx::query(
        r#"
        UPDATE tracks
        SET title = $1,
            duration_ms = $2,
            album = $3,
            album_source = $4,
            updated_at = CURRENT_TIMESTAMP
        WHERE id = $5
        "#,
    )
    .bind(metadata.title())
    .bind(encode_duration_ms(metadata.duration_ms())?)
    .bind(album.as_deref())
    .bind(album_source.as_deref())
    .bind(track_id.as_uuid())
    .execute(&mut *connection)
    .await
    .map_err(|_| TrackRepositoryError::Unavailable)?;

    if result.rows_affected() != 1 {
        return Err(TrackRepositoryError::InvalidData);
    }

    Ok(album)
}

async fn replace_artist_credits(
    connection: &mut PgConnection,
    track_id: &TrackId,
    metadata: &TrackMetadata,
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
    .map_err(|_| TrackRepositoryError::Unavailable)?;

    for (position, artist) in metadata.artists().iter().enumerate() {
        let position = i32::try_from(position).map_err(|_| TrackRepositoryError::InvalidData)?;

        sqlx::query(
            r#"
            INSERT INTO track_artist_credits (track_id, position, name)
            VALUES ($1, $2, $3)
            "#,
        )
        .bind(track_id.as_uuid())
        .bind(position)
        .bind(artist)
        .execute(&mut *connection)
        .await
        .map_err(|_| TrackRepositoryError::Unavailable)?;
    }

    Ok(())
}

fn encode_duration_ms(duration_ms: Option<u64>) -> Result<Option<i64>, TrackRepositoryError> {
    duration_ms
        .map(i64::try_from)
        .transpose()
        .map_err(|_| TrackRepositoryError::InvalidData)
}

fn encode_source_scope(source_scope: &SourceScope) -> String {
    match source_scope {
        SourceScope::Global => "global".to_owned(),
        SourceScope::Named(name) => format!("named:{}", name),
    }
}
