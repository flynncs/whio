use std::sync::Arc;

use thiserror::Error;
use uuid::Uuid;

use crate::features::music_library::{
    domain::{
        ListenCount, ListenOutcome, ListenPage, ListenQuery, ListenSubmission, ListenWindow,
        TrackId,
    },
    storage::contracts::{LibraryRead, ListenStore, TrackRepositoryError},
};

#[derive(Debug, Error)]
pub enum ListenError {
    #[error("listen named an unknown track")]
    UnknownTrack,

    #[error("listen repository failed")]
    Repository(#[source] TrackRepositoryError),

    #[error("listen storage is not configured")]
    Disabled,
}

pub struct ListenService {
    store: Option<Arc<dyn ListenStore>>,
    library: Option<Arc<dyn LibraryRead>>,
}

impl ListenService {
    pub fn new(store: Arc<dyn ListenStore>, library: Arc<dyn LibraryRead>) -> Self {
        Self {
            store: Some(store),
            library: Some(library),
        }
    }

    pub fn disabled() -> Self {
        Self {
            store: None,
            library: None,
        }
    }

    pub async fn submit_listen(
        &self,
        submission: &ListenSubmission,
    ) -> Result<ListenOutcome, ListenError> {
        let known = self
            .library
            .as_ref()
            .ok_or(ListenError::Disabled)?
            .find_track(submission.track_id())
            .await
            .map_err(ListenError::Repository)?
            .is_some();
        if !known {
            return Err(ListenError::UnknownTrack);
        }

        self.store
            .as_ref()
            .ok_or(ListenError::Disabled)?
            .submit_listen(submission)
            .await
            .map_err(ListenError::Repository)
    }

    pub async fn history(
        &self,
        principal: Uuid,
        query: &ListenQuery,
    ) -> Result<ListenPage, ListenError> {
        self.store
            .as_ref()
            .ok_or(ListenError::Disabled)?
            .listen_history(principal, query)
            .await
            .map_err(ListenError::Repository)
    }

    pub async fn count(
        &self,
        principal: Uuid,
        window: ListenWindow,
        track: Option<&TrackId>,
    ) -> Result<ListenCount, ListenError> {
        self.store
            .as_ref()
            .ok_or(ListenError::Disabled)?
            .listen_count(principal, window, track)
            .await
            .map_err(ListenError::Repository)
    }
}
