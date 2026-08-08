use super::{SourceIdentity, TrackId};

#[derive(Debug, thiserror::Error)]
pub enum TrackRepositoryError {
    #[error("track repository is unavailable")]
    Unavailable,
}

pub trait TrackRepository: Send + Sync {
    fn get_or_create_id(&self, source: SourceIdentity) -> Result<TrackId, TrackRepositoryError>;

    fn find_source(
        &self,
        track_id: &TrackId,
    ) -> Result<Option<SourceIdentity>, TrackRepositoryError>;
}
