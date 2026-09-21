use async_trait::async_trait;
use uuid::Uuid;

use crate::tracks::{LibraryState, Starred, StateTarget, TrackId};

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
        _artist_id: &crate::tracks::ArtistId,
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

    async fn record_play(
        &self,
        _user: Uuid,
        _track_id: &TrackId,
        _played_at: chrono::DateTime<chrono::Utc>,
    ) -> Result<(), TrackRepositoryError> {
        Ok(())
    }
}

#[async_trait]
pub trait DuplicateReport: Send + Sync {
    async fn duplicate_candidates(
        &self,
        _limit: u32,
    ) -> Result<Vec<crate::tracks::DuplicateCandidate>, TrackRepositoryError> {
        Ok(Vec::new())
    }

    async fn split_artists(
        &self,
        _limit: u32,
    ) -> Result<Vec<crate::tracks::SplitArtist>, TrackRepositoryError> {
        Ok(Vec::new())
    }
}
