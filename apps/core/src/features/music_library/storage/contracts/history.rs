use async_trait::async_trait;
use uuid::Uuid;

use crate::features::music_library::domain::{
    ListenCount, ListenOutcome, ListenPage, ListenQuery, ListenSubmission, ListenWindow, TrackId,
};

use super::TrackRepositoryError;

#[async_trait]
pub trait ListenStore: Send + Sync {
    async fn submit_listen(
        &self,
        submission: &ListenSubmission,
    ) -> Result<ListenOutcome, TrackRepositoryError>;

    async fn listen_history(
        &self,
        principal: Uuid,
        query: &ListenQuery,
    ) -> Result<ListenPage, TrackRepositoryError>;

    async fn listen_count(
        &self,
        principal: Uuid,
        window: ListenWindow,
        track: Option<&TrackId>,
    ) -> Result<ListenCount, TrackRepositoryError>;
}
