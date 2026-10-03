use async_trait::async_trait;
use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::features::music_library::{
    artwork::{ArtworkAssetDescriptor, ArtworkRole},
    domain::ArtistId,
};

use super::TrackRepositoryError;

#[async_trait]
pub trait ArtworkStore: Send + Sync {
    async fn find_ready(
        &self,
        subject: &ArtistId,
        role: ArtworkRole,
    ) -> Result<Vec<ArtworkAssetDescriptor>, TrackRepositoryError>;

    async fn record_asset(
        &self,
        tx: &mut sqlx::PgConnection,
        asset: &ArtworkAssetDescriptor,
    ) -> Result<(), TrackRepositoryError>;

    async fn record_assets(
        &self,
        assets: &[ArtworkAssetDescriptor],
    ) -> Result<(), TrackRepositoryError>;

    async fn touch(&self, id: Uuid, now: DateTime<Utc>) -> Result<(), TrackRepositoryError>;

    async fn lru_descriptors(
        &self,
        exclude: Option<Uuid>,
        limit: u32,
    ) -> Result<Vec<ArtworkAssetDescriptor>, TrackRepositoryError>;

    async fn delete_asset(&self, id: Uuid) -> Result<bool, TrackRepositoryError>;
}
