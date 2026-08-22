use async_trait::async_trait;
use sqlx::{PgConnection, PgPool};
use uuid::Uuid;

use crate::tracks::{
    ProviderId, SourceIdentity, SourceScope, Track, TrackId, TrackMetadata, TrackRepository,
    TrackRepositoryError,
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

            insert_candidate_track(&mut tx, &candidate_track_id, &metadata).await?;

            if try_insert_source(&mut tx, &candidate_track_id, &source).await? {
                candidate_track_id
            } else {
                resolve_concurrent_winner(&mut tx, &candidate_track_id, &source).await?
            }
        };

        update_track_metadata(&mut tx, &track_id, &metadata).await?;
        replace_artist_credits(&mut tx, &track_id, &metadata).await?;

        tx.commit()
            .await
            .map_err(|_| TrackRepositoryError::Unavailable)?;

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
            })
            .collect()
    }
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
    metadata: &TrackMetadata,
) -> Result<(), TrackRepositoryError> {
    sqlx::query(
        r#"
        INSERT INTO tracks (id, title, duration_ms)
        VALUES ($1, $2, $3)
        "#,
    )
    .bind(track_id.as_uuid())
    .bind(metadata.title())
    .bind(encode_duration_ms(metadata.duration_ms())?)
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
            updated_at = CURRENT_TIMESTAMP
        WHERE id = $3
        "#,
    )
    .bind(metadata.title())
    .bind(encode_duration_ms(metadata.duration_ms())?)
    .bind(track_id.as_uuid())
    .execute(&mut *connection)
    .await
    .map_err(|_| TrackRepositoryError::Unavailable)?;

    if result.rows_affected() != 1 {
        return Err(TrackRepositoryError::InvalidData);
    }

    Ok(())
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
