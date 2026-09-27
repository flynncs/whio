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
}

pub struct ListenService {
    store: Arc<dyn ListenStore>,
    library: Arc<dyn LibraryRead>,
}

impl ListenService {
    pub fn new(store: Arc<dyn ListenStore>, library: Arc<dyn LibraryRead>) -> Self {
        Self { store, library }
    }

    pub async fn submit_listen(
        &self,
        submission: &ListenSubmission,
    ) -> Result<ListenOutcome, ListenError> {
        let known = self
            .library
            .find_track(submission.track_id())
            .await
            .map_err(ListenError::Repository)?
            .is_some();
        if !known {
            return Err(ListenError::UnknownTrack);
        }

        self.store
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
            .listen_count(principal, window, track)
            .await
            .map_err(ListenError::Repository)
    }
}
