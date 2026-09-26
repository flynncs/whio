use std::sync::Arc;

use thiserror::Error;
use uuid::Uuid;

use crate::{
    features::music_library::catalogue::{CatalogueError, LibraryService, SearchHit},
    features::music_library::{
        domain::{Playlist, PlaylistId, TrackId},
        storage::contracts::{PlaylistStore, TrackRepositoryError},
    },
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

pub struct PlaylistEdit<'a> {
    pub name: Option<&'a str>,
    pub comment: Option<&'a str>,
    pub public: Option<bool>,
    pub additions: &'a [TrackId],
    pub removals: Vec<usize>,
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

    /// Preserve the existing incremental edit contract: metadata errors fail the
    /// request, while invalid/unavailable individual entry edits do not undo it.
    pub async fn edit(
        &self,
        user: Uuid,
        playlist_id: &PlaylistId,
        mut edit: PlaylistEdit<'_>,
    ) -> Result<bool, PlaylistError> {
        let Some(current) = self.detail(user, playlist_id).await? else {
            return Ok(false);
        };
        let name = edit.name.unwrap_or_else(|| current.playlist.name());
        let comment = edit.comment.or_else(|| current.playlist.comment());
        let public = edit.public.unwrap_or_else(|| current.playlist.is_public());
        self.update(user, playlist_id, name, comment, public)
            .await?;
        for track in edit.additions {
            let _ = self.add_track(user, playlist_id, track).await;
        }
        edit.removals
            .sort_unstable_by(|left, right| right.cmp(left));
        for index in edit.removals {
            let _ = self.remove_index(user, playlist_id, index).await;
        }
        Ok(true)
    }

    pub async fn create_or_append(
        &self,
        user: Uuid,
        playlist_id: Option<PlaylistId>,
        name: &str,
        additions: &[TrackId],
    ) -> Result<Option<PlaylistDetail>, PlaylistError> {
        let playlist_id = match playlist_id {
            Some(id) => {
                if !name.is_empty() {
                    self.update(user, &id, name, None, false).await?;
                }
                id
            }
            None => self.create(user, name, None).await?.id().clone(),
        };
        for track in additions {
            let _ = self.add_track(user, &playlist_id, track).await;
        }
        self.detail(user, &playlist_id).await
    }

    fn store(&self) -> Result<&Arc<dyn PlaylistStore>, PlaylistError> {
        self.store.as_ref().ok_or(PlaylistError::Disabled)
    }
}
