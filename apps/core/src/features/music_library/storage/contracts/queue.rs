use async_trait::async_trait;
use uuid::Uuid;

use crate::features::music_library::domain::{
    RestoreQueueSnapshot, RestoreQueueWrite, RestoreQueueWriteOutcome,
};

use super::TrackRepositoryError;

#[async_trait]
pub trait RestoreQueueStore: Send + Sync {
    async fn read_restore_queue(
        &self,
        user: Uuid,
    ) -> Result<Option<RestoreQueueSnapshot>, TrackRepositoryError>;

    async fn save_restore_queue(
        &self,
        write: RestoreQueueWrite,
    ) -> Result<RestoreQueueWriteOutcome, TrackRepositoryError>;

    async fn clear_restore_queue(&self, user: Uuid) -> Result<bool, TrackRepositoryError>;
}
