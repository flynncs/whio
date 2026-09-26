use async_trait::async_trait;
use uuid::Uuid;

use crate::features::music_library::domain::{
    Playlist, PlaylistId, PlaylistOccurrence, PlaylistWrite, PlaylistWriteOutcome, TrackId,
};

use super::TrackRepositoryError;

#[async_trait]
pub trait PlaylistStore: Send + Sync {
    async fn create_playlist(
        &self,
        owner: Uuid,
        name: &str,
        comment: Option<&str>,
    ) -> Result<Playlist, TrackRepositoryError>;

    async fn find_playlist(
        &self,
        playlist_id: &PlaylistId,
    ) -> Result<Option<Playlist>, TrackRepositoryError>;

    async fn list_playlists(&self, principal: Uuid) -> Result<Vec<Playlist>, TrackRepositoryError>;

    async fn delete_playlist(
        &self,
        owner: Uuid,
        playlist_id: &PlaylistId,
    ) -> Result<bool, TrackRepositoryError>;

    async fn playlist_occurrences(
        &self,
        playlist_id: &PlaylistId,
    ) -> Result<Vec<PlaylistOccurrence>, TrackRepositoryError>;

    async fn write_playlist(
        &self,
        write: PlaylistWrite,
    ) -> Result<PlaylistWriteOutcome, TrackRepositoryError>;

    async fn add_playlist_track(
        &self,
        owner: Uuid,
        playlist_id: &PlaylistId,
        track_id: &TrackId,
    ) -> Result<bool, TrackRepositoryError>;
}
