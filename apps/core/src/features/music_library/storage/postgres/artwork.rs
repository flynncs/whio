use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::PgConnection;
use uuid::Uuid;

use crate::features::music_library::{
    artwork::{ArtworkAssetDescriptor, ArtworkRole, ArtworkVariant},
    domain::ArtistId,
    storage::contracts::{ArtworkStore, TrackRepositoryError},
};

use super::PgStore;
use super::decode::repository_error;

const ASSET_COLUMNS: &str = "id, subject_id, role, source_provider_id, source_external_id, \
     revision, variant, content_digest, content_type, width, height, byte_size, file_name";

type AssetRow = (
    Uuid,
    Uuid,
    String,
    String,
    String,
    i64,
    String,
    String,
    String,
    i32,
    i32,
    i64,
    String,
);

fn decode_asset(row: AssetRow) -> Result<ArtworkAssetDescriptor, TrackRepositoryError> {
    let (
        id,
        subject_id,
        role,
        source_provider_id,
        source_external_id,
        revision,
        variant,
        content_digest,
        content_type,
        width,
        height,
        byte_size,
        file_name,
    ) = row;

    Ok(ArtworkAssetDescriptor {
        id,
        subject: ArtistId::from_uuid(subject_id),
        role: ArtworkRole::parse(&role).ok_or(TrackRepositoryError::InvalidData)?,
        source_provider_id,
        source_external_id,
        revision,
        variant: ArtworkVariant::parse(&variant).ok_or(TrackRepositoryError::InvalidData)?,
        content_digest,
        content_type,
        width: u32::try_from(width).map_err(|_| TrackRepositoryError::InvalidData)?,
        height: u32::try_from(height).map_err(|_| TrackRepositoryError::InvalidData)?,
        byte_size,
        file_name,
    })
}

#[async_trait]
impl ArtworkStore for PgStore {
    async fn find_ready(
        &self,
        subject: &ArtistId,
        role: ArtworkRole,
    ) -> Result<Vec<ArtworkAssetDescriptor>, TrackRepositoryError> {
        let rows = sqlx::query_as::<_, AssetRow>(&format!(
            "SELECT {ASSET_COLUMNS} FROM artwork_assets \
             WHERE subject_kind = 'artist' AND subject_id = $1 AND role = $2 \
             ORDER BY revision DESC, variant, id"
        ))
        .bind(subject.as_uuid())
        .bind(role.as_str())
        .fetch_all(&self.pool)
        .await
        .map_err(repository_error)?;

        rows.into_iter().map(decode_asset).collect()
    }

    async fn record_asset(
        &self,
        tx: &mut PgConnection,
        asset: &ArtworkAssetDescriptor,
    ) -> Result<(), TrackRepositoryError> {
        sqlx::query(
            "INSERT INTO artwork_assets (id, subject_kind, subject_id, role, source_provider_id, \
             source_external_id, revision, variant, content_digest, content_type, width, height, \
             byte_size, file_name, fetched_at, last_used_at) \
             VALUES ($1, 'artist', $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, \
             clock_timestamp(), clock_timestamp()) \
             ON CONFLICT (subject_kind, subject_id, role, source_provider_id, source_external_id, \
             revision, variant) DO NOTHING",
        )
        .bind(asset.id)
        .bind(asset.subject.as_uuid())
        .bind(asset.role.as_str())
        .bind(&asset.source_provider_id)
        .bind(&asset.source_external_id)
        .bind(asset.revision)
        .bind(asset.variant.as_str())
        .bind(&asset.content_digest)
        .bind(&asset.content_type)
        .bind(i32::try_from(asset.width).map_err(|_| TrackRepositoryError::InvalidData)?)
        .bind(i32::try_from(asset.height).map_err(|_| TrackRepositoryError::InvalidData)?)
        .bind(asset.byte_size)
        .bind(&asset.file_name)
        .execute(&mut *tx)
        .await
        .map(|_| ())
        .map_err(repository_error)
    }

    async fn record_assets(
        &self,
        assets: &[ArtworkAssetDescriptor],
    ) -> Result<(), TrackRepositoryError> {
        let mut transaction = self.pool.begin().await.map_err(repository_error)?;

        for asset in assets {
            self.record_asset(&mut transaction, asset).await?;
        }

        transaction.commit().await.map_err(repository_error)
    }

    async fn touch(&self, id: Uuid, now: DateTime<Utc>) -> Result<(), TrackRepositoryError> {
        sqlx::query("UPDATE artwork_assets SET last_used_at = $2 WHERE id = $1")
            .bind(id)
            .bind(now)
            .execute(&self.pool)
            .await
            .map(|_| ())
            .map_err(repository_error)
    }

    async fn lru_descriptors(
        &self,
        exclude: Option<Uuid>,
        limit: u32,
    ) -> Result<Vec<ArtworkAssetDescriptor>, TrackRepositoryError> {
        let rows = sqlx::query_as::<_, AssetRow>(&format!(
            "SELECT {ASSET_COLUMNS} FROM artwork_assets \
             WHERE ($1::UUID IS NULL OR id <> $1::UUID) \
             ORDER BY last_used_at, id LIMIT $2"
        ))
        .bind(exclude)
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await
        .map_err(repository_error)?;

        rows.into_iter().map(decode_asset).collect()
    }

    async fn delete_asset(&self, id: Uuid) -> Result<bool, TrackRepositoryError> {
        let deleted = sqlx::query("DELETE FROM artwork_assets WHERE id = $1")
            .bind(id)
            .execute(&self.pool)
            .await
            .map_err(repository_error)?;

        Ok(deleted.rows_affected() == 1)
    }
}
