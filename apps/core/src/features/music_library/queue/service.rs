use std::sync::Arc;

use thiserror::Error;
use uuid::Uuid;

use crate::features::music_library::{
    domain::{
        RestoreQueueSnapshot, RestoreQueueValidationError, RestoreQueueWrite,
        RestoreQueueWriteOutcome, TrackId,
    },
    storage::contracts::{RestoreQueueStore, TrackRepositoryError},
};

#[derive(Debug, Error)]
pub enum RestoreQueueError {
    #[error("the restore queue is not configured")]
    Disabled,

    #[error("restore queue repository failed")]
    Repository(#[source] TrackRepositoryError),

    #[error("restore queue has revision {current_revision}, not the requested revision")]
    Conflict { current_revision: i64 },

    #[error(transparent)]
    Invalid(#[from] RestoreQueueValidationError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreQueueSave {
    pub entries: Vec<TrackId>,
    pub current_index: Option<u32>,
    pub position_ms: u64,
}

pub struct RestoreQueueService {
    store: Option<Arc<dyn RestoreQueueStore>>,
}

impl RestoreQueueService {
    pub fn new(store: Arc<dyn RestoreQueueStore>) -> Self {
        Self { store: Some(store) }
    }

    pub fn disabled() -> Self {
        Self { store: None }
    }

    pub async fn read(
        &self,
        principal: Uuid,
    ) -> Result<Option<RestoreQueueSnapshot>, RestoreQueueError> {
        self.store()?
            .read_restore_queue(principal)
            .await
            .map_err(RestoreQueueError::Repository)
    }

    pub async fn save(
        &self,
        principal: Uuid,
        save: RestoreQueueSave,
    ) -> Result<RestoreQueueSnapshot, RestoreQueueError> {
        self.save_at_revision(principal, save, None).await
    }

    pub async fn save_at_revision(
        &self,
        principal: Uuid,
        save: RestoreQueueSave,
        revision: Option<i64>,
    ) -> Result<RestoreQueueSnapshot, RestoreQueueError> {
        let write = RestoreQueueWrite {
            user_id: principal,
            expected_revision: revision,
            entries: save.entries,
            current_index: save.current_index,
            position_ms: save.position_ms,
        };

        saved(
            self.store()?
                .save_restore_queue(write)
                .await
                .map_err(RestoreQueueError::Repository)?,
        )
    }

    pub async fn clear(&self, principal: Uuid) -> Result<bool, RestoreQueueError> {
        self.store()?
            .clear_restore_queue(principal)
            .await
            .map_err(RestoreQueueError::Repository)
    }

    fn store(&self) -> Result<&Arc<dyn RestoreQueueStore>, RestoreQueueError> {
        self.store.as_ref().ok_or(RestoreQueueError::Disabled)
    }
}

fn saved(outcome: RestoreQueueWriteOutcome) -> Result<RestoreQueueSnapshot, RestoreQueueError> {
    match outcome {
        RestoreQueueWriteOutcome::Saved(snapshot) => Ok(snapshot),
        RestoreQueueWriteOutcome::RevisionConflict { current_revision } => {
            Err(RestoreQueueError::Conflict { current_revision })
        }
        RestoreQueueWriteOutcome::Rejected(error) => Err(error.into()),
    }
}
