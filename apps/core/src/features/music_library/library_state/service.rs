use std::sync::Arc;

use uuid::Uuid;

use crate::features::music_library::{
    domain::{AlbumId, ArtistId, Starred, StateTarget, TrackId},
    storage::contracts::{LibraryRead, StateStore, TrackRepositoryError},
};

#[derive(Debug, thiserror::Error)]
pub enum LibraryStateError {
    #[error("track repository failed")]
    Repository(#[source] TrackRepositoryError),
}

pub struct LibraryStateService {
    state: Option<Arc<dyn StateStore>>,
    library: Option<Arc<dyn LibraryRead>>,
}

impl LibraryStateService {
    pub fn new(state: Arc<dyn StateStore>, library: Arc<dyn LibraryRead>) -> Self {
        Self {
            state: Some(state),
            library: Some(library),
        }
    }

    pub fn disabled() -> Self {
        Self {
            state: None,
            library: None,
        }
    }

    pub async fn starred(&self, user: Uuid) -> Result<Starred, LibraryStateError> {
        let Some(repository) = &self.state else {
            return Ok(Starred::new(Vec::new(), Vec::new(), Vec::new()));
        };

        repository
            .starred(user)
            .await
            .map_err(LibraryStateError::Repository)
    }

    pub async fn set_starred(
        &self,
        user: Uuid,
        target: StateTarget<'_>,
        starred: bool,
    ) -> Result<(), LibraryStateError> {
        let (Some(state), Some(library)) = (&self.state, &self.library) else {
            return Ok(());
        };

        let exists = match target {
            StateTarget::Track(track_id) => library
                .find_track(track_id)
                .await
                .map_err(LibraryStateError::Repository)?
                .is_some(),
            StateTarget::Album(album_id) => library
                .find_album(album_id)
                .await
                .map_err(LibraryStateError::Repository)?
                .is_some(),
            StateTarget::Artist(artist_id) => library
                .find_artist(artist_id, Some(user))
                .await
                .map_err(LibraryStateError::Repository)?
                .is_some(),
        };
        if !exists {
            return Ok(());
        }

        state
            .set_starred(user, target, starred)
            .await
            .map_err(LibraryStateError::Repository)
    }

    pub async fn set_rating(
        &self,
        user: Uuid,
        target: StateTarget<'_>,
        rating: Option<u8>,
    ) -> Result<(), LibraryStateError> {
        let Some(repository) = &self.state else {
            return Ok(());
        };

        repository
            .set_rating(user, target, rating)
            .await
            .map_err(LibraryStateError::Repository)
    }

    pub async fn rate_id(
        &self,
        user: Uuid,
        raw_id: &str,
        rating: Option<u8>,
    ) -> Result<bool, LibraryStateError> {
        let (Some(_), Some(library)) = (&self.state, &self.library) else {
            return Ok(false);
        };

        if let Ok(track_id) = raw_id.parse::<TrackId>()
            && library
                .find_track(&track_id)
                .await
                .map_err(LibraryStateError::Repository)?
                .is_some()
        {
            self.set_rating(user, StateTarget::Track(&track_id), rating)
                .await?;
            return Ok(true);
        }
        if let Ok(album_id) = raw_id.parse::<AlbumId>()
            && library
                .find_album(&album_id)
                .await
                .map_err(LibraryStateError::Repository)?
                .is_some()
        {
            self.set_rating(user, StateTarget::Album(&album_id), rating)
                .await?;
            return Ok(true);
        }
        if let Ok(artist_id) = raw_id.parse::<ArtistId>()
            && library
                .find_artist(&artist_id, Some(user))
                .await
                .map_err(LibraryStateError::Repository)?
                .is_some()
        {
            self.set_rating(user, StateTarget::Artist(&artist_id), rating)
                .await?;
            return Ok(true);
        }

        Ok(false)
    }
}

#[cfg(all(test, feature = "postgres-integration"))]
#[path = "service_tests.rs"]
mod tests;
