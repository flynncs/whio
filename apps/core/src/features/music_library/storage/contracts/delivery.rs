use async_trait::async_trait;
use sqlx::PgConnection;
use uuid::Uuid;

use crate::features::music_library::delivery::{
    DeliveryFailure, DeliveryRow, DeliveryState, DeliveryStatus,
};

use super::TrackRepositoryError;

#[async_trait]
pub trait DeliveryStore: Send + Sync {
    async fn insert_pending(
        &self,
        tx: &mut PgConnection,
        row: &DeliveryRow,
    ) -> Result<(), TrackRepositoryError>;

    async fn find(&self, delivery_id: Uuid) -> Result<Option<DeliveryRow>, TrackRepositoryError>;

    async fn transition(
        &self,
        delivery_id: Uuid,
        from: DeliveryState,
        to: DeliveryState,
        attempts: i32,
        provider_reference: Option<&str>,
        failure: Option<&DeliveryFailure>,
    ) -> Result<bool, TrackRepositoryError>;

    async fn list_for_user(
        &self,
        user_id: Uuid,
        state: Option<DeliveryState>,
        limit: u32,
    ) -> Result<Vec<DeliveryStatus>, TrackRepositoryError>;
}
