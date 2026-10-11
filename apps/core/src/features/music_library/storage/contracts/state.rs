use async_trait::async_trait;
use uuid::Uuid;

use crate::features::music_library::domain::{
    EvictionReport, LibraryState, ProviderDuplicateReport, RetentionReport, Starred, StateTarget,
    TrackId,
};

use super::TrackRepositoryError;

#[async_trait]
pub trait StateStore: Send + Sync {
    async fn find_track_state(
        &self,
        _track_id: &TrackId,
        _user: Uuid,
    ) -> Result<LibraryState, TrackRepositoryError> {
        Ok(LibraryState::default())
    }

    async fn find_artist_state(
        &self,
        _artist_id: &crate::features::music_library::domain::ArtistId,
        _user: Uuid,
    ) -> Result<LibraryState, TrackRepositoryError> {
        Ok(LibraryState::default())
    }

    async fn starred(&self, _user: Uuid) -> Result<Starred, TrackRepositoryError> {
        Ok(Starred::new(Vec::new(), Vec::new(), Vec::new()))
    }

    async fn set_starred(
        &self,
        _user: Uuid,
        _target: StateTarget<'_>,
        _starred: bool,
    ) -> Result<(), TrackRepositoryError> {
        Ok(())
    }

    async fn set_rating(
        &self,
        _user: Uuid,
        _target: StateTarget<'_>,
        _rating: Option<u8>,
    ) -> Result<(), TrackRepositoryError> {
        Ok(())
    }
}

#[async_trait]
pub trait DuplicateReport: Send + Sync {
    async fn duplicate_candidates(
        &self,
        _limit: u32,
    ) -> Result<Vec<crate::features::music_library::domain::DuplicateCandidate>, TrackRepositoryError>
    {
        Ok(Vec::new())
    }

    async fn split_artists(
        &self,
        _limit: u32,
    ) -> Result<Vec<crate::features::music_library::domain::SplitArtist>, TrackRepositoryError>
    {
        Ok(Vec::new())
    }
}

#[async_trait]
pub trait Retention: Send + Sync {
    async fn retention_report(&self) -> Result<RetentionReport, TrackRepositoryError> {
        Ok(RetentionReport::default())
    }

    async fn evict_unengaged(&self) -> Result<EvictionReport, TrackRepositoryError> {
        Ok(EvictionReport::default())
    }

    async fn provider_duplicates(&self) -> Result<ProviderDuplicateReport, TrackRepositoryError>;

    async fn delete_provider_duplicates(
        &self,
    ) -> Result<ProviderDuplicateReport, TrackRepositoryError>;
}
