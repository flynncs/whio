use std::sync::Arc;

use thiserror::Error;
use uuid::Uuid;

use crate::{
    catalogue::{CatalogueError, LibraryService, SearchHit},
    tracks::{Playlist, PlaylistId, PlaylistStore, TrackId, TrackRepositoryError},
};

#[derive(Debug, Error)]
pub enum PlaylistError {
    #[error("playlists are not configured")]
    Disabled,

    #[error("playlist repository failed")]
    Repository(#[source] TrackRepositoryError),

    #[error("playlist entries could not be read")]
    Library(#[source] CatalogueError),
}

pub struct PlaylistDetail {
    pub playlist: Playlist,
    pub entries: Vec<SearchHit>,
}

pub struct PlaylistService {
    store: Option<Arc<dyn PlaylistStore>>,
    library: Arc<LibraryService>,
}

impl PlaylistService {
    pub fn new(store: Arc<dyn PlaylistStore>, library: Arc<LibraryService>) -> Self {
        Self {
            store: Some(store),
            library,
        }
    }

    pub fn disabled(library: Arc<LibraryService>) -> Self {
        Self {
            store: None,
            library,
        }
    }

    pub async fn list(&self, user: Uuid) -> Result<Vec<Playlist>, PlaylistError> {
        self.store()?
            .list_playlists(user)
            .await
            .map_err(PlaylistError::Repository)
    }

    pub async fn detail(
        &self,
        user: Uuid,
        playlist_id: &PlaylistId,
    ) -> Result<Option<PlaylistDetail>, PlaylistError> {
        let store = self.store()?;
        let Some(playlist) = store
            .find_playlist(user, playlist_id)
            .await
            .map_err(PlaylistError::Repository)?
        else {
            return Ok(None);
        };

        let track_ids = store
            .playlist_tracks(playlist_id)
            .await
            .map_err(PlaylistError::Repository)?;
        let mut entries = Vec::with_capacity(track_ids.len());
        for track_id in track_ids {
            if let Some(hit) = self
                .library
                .track_detail(&track_id, Some(&user))
                .await
                .map_err(PlaylistError::Library)?
            {
                entries.push(hit);
            }
        }

        Ok(Some(PlaylistDetail { playlist, entries }))
    }

    pub async fn create(
        &self,
        user: Uuid,
        name: &str,
        comment: Option<&str>,
    ) -> Result<Playlist, PlaylistError> {
        self.store()?
            .create_playlist(user, name, comment)
            .await
            .map_err(PlaylistError::Repository)
    }

    pub async fn update(
        &self,
        user: Uuid,
        playlist_id: &PlaylistId,
        name: &str,
        comment: Option<&str>,
        public: bool,
    ) -> Result<Option<Playlist>, PlaylistError> {
        self.store()?
            .update_playlist(user, playlist_id, name, comment, public)
            .await
            .map_err(PlaylistError::Repository)
    }

    pub async fn delete(
        &self,
        user: Uuid,
        playlist_id: &PlaylistId,
    ) -> Result<bool, PlaylistError> {
        self.store()?
            .delete_playlist(user, playlist_id)
            .await
            .map_err(PlaylistError::Repository)
    }

    pub async fn add_track(
        &self,
        user: Uuid,
        playlist_id: &PlaylistId,
        track_id: &TrackId,
    ) -> Result<bool, PlaylistError> {
        self.store()?
            .add_playlist_track(user, playlist_id, track_id)
            .await
            .map_err(PlaylistError::Repository)
    }

    pub async fn remove_index(
        &self,
        user: Uuid,
        playlist_id: &PlaylistId,
        index: usize,
    ) -> Result<bool, PlaylistError> {
        let store = self.store()?;
        let tracks = store
            .playlist_tracks(playlist_id)
            .await
            .map_err(PlaylistError::Repository)?;
        let Some(track_id) = tracks.get(index).cloned() else {
            return Ok(false);
        };

        store
            .remove_playlist_track(user, playlist_id, &track_id)
            .await
            .map_err(PlaylistError::Repository)
    }

    fn store(&self) -> Result<&Arc<dyn PlaylistStore>, PlaylistError> {
        self.store.as_ref().ok_or(PlaylistError::Disabled)
    }
}
