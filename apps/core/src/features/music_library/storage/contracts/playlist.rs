use async_trait::async_trait;
use uuid::Uuid;

use crate::features::music_library::domain::{Playlist, PlaylistId, TrackId};

use super::TrackRepositoryError;

#[async_trait]
pub trait PlaylistStore: Send + Sync {
    async fn create_playlist(
        &self,
        _user: Uuid,
        _name: &str,
        _comment: Option<&str>,
    ) -> Result<Playlist, TrackRepositoryError> {
        Err(TrackRepositoryError::Unavailable)
    }

    async fn update_playlist(
        &self,
        _user: Uuid,
        _playlist_id: &PlaylistId,
        _name: &str,
        _comment: Option<&str>,
        _public: bool,
    ) -> Result<Option<Playlist>, TrackRepositoryError> {
        Ok(None)
    }

    async fn delete_playlist(
        &self,
        _user: Uuid,
        _playlist_id: &PlaylistId,
    ) -> Result<bool, TrackRepositoryError> {
        Ok(false)
    }

    async fn find_playlist(
        &self,
        _user: Uuid,
        _playlist_id: &PlaylistId,
    ) -> Result<Option<Playlist>, TrackRepositoryError> {
        Ok(None)
    }

    async fn list_playlists(&self, _user: Uuid) -> Result<Vec<Playlist>, TrackRepositoryError> {
        Ok(Vec::new())
    }

    async fn playlist_tracks(
        &self,
        _playlist_id: &PlaylistId,
    ) -> Result<Vec<TrackId>, TrackRepositoryError> {
        Ok(Vec::new())
    }

    async fn add_playlist_track(
        &self,
        _user: Uuid,
        _playlist_id: &PlaylistId,
        _track_id: &TrackId,
    ) -> Result<bool, TrackRepositoryError> {
        Ok(false)
    }

    async fn remove_playlist_track(
        &self,
        _user: Uuid,
        _playlist_id: &PlaylistId,
        _track_id: &TrackId,
    ) -> Result<bool, TrackRepositoryError> {
        Ok(false)
    }
}
