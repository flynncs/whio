use async_trait::async_trait;

use crate::tracks::{Track, TrackMetadata};

use super::{SourceIdentity, TrackId};

#[derive(Debug, thiserror::Error)]
pub enum TrackRepositoryError {
    #[error("track repository is unavailable")]
    Unavailable,
    #[error("track repository contained invalid data")]
    InvalidData,
}

#[async_trait]
pub trait TrackRepository: Send + Sync {
    async fn get_or_create(
        &self,
        source: SourceIdentity,
        metadata: TrackMetadata,
    ) -> Result<Track, TrackRepositoryError>;

    async fn find_sources(
        &self,
        track_id: &TrackId,
    ) -> Result<Vec<SourceIdentity>, TrackRepositoryError>;
}
