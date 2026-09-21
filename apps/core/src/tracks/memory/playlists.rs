use async_trait::async_trait;
use uuid::Uuid;

use crate::tracks::{Playlist, PlaylistId, PlaylistStore, TrackId, TrackRepositoryError};

use super::*;
use super::{InMemoryTrackRepository, StoredPlaylist};

fn locked(
    repository: &InMemoryTrackRepository,
) -> Result<std::sync::MutexGuard<'_, TrackIndexes>, TrackRepositoryError> {
    repository
        .indexes
        .lock()
        .map_err(|_| TrackRepositoryError::Unavailable)
}

fn track_count(indexes: &TrackIndexes, playlist_id: &PlaylistId) -> u32 {
    indexes
        .playlist_tracks
        .get(playlist_id)
        .map_or(0, |tracks| tracks.len() as u32)
}

fn duration_ms(indexes: &TrackIndexes, playlist_id: &PlaylistId) -> u64 {
    indexes
        .playlist_tracks
        .get(playlist_id)
        .map_or(0, |tracks| {
            tracks
                .iter()
                .filter_map(|track_id| indexes.by_track_id.get(track_id))
                .filter_map(|entry| entry.track.duration_ms())
                .fold(0u64, u64::saturating_add)
        })
}

#[async_trait]
impl PlaylistStore for InMemoryTrackRepository {
    async fn create_playlist(
        &self,
        user: Uuid,
        name: &str,
        comment: Option<&str>,
    ) -> Result<Playlist, TrackRepositoryError> {
        let mut indexes = locked(self)?;
        let now = chrono::Utc::now();
        let id = PlaylistId::new();
        let playlist = Playlist::new(
            id.clone(),
            name.to_owned(),
            comment.map(str::to_owned),
            false,
            now,
            now,
            0,
            0,
        )
        .map_err(|_| TrackRepositoryError::InvalidData)?;

        indexes.playlists.insert(
            id.clone(),
            StoredPlaylist {
                user,
                name: playlist.name().to_owned(),
                comment: playlist.comment().map(str::to_owned),
                public: false,
                created_at: now,
                updated_at: now,
            },
        );
        indexes.playlist_tracks.insert(id, Vec::new());

        Ok(playlist)
    }

    async fn update_playlist(
        &self,
        user: Uuid,
        playlist_id: &PlaylistId,
        name: &str,
        comment: Option<&str>,
        public: bool,
    ) -> Result<Option<Playlist>, TrackRepositoryError> {
        let mut indexes = locked(self)?;
        let count = track_count(&indexes, playlist_id);
        let duration = duration_ms(&indexes, playlist_id);

        let Some(stored) = indexes.playlists.get_mut(playlist_id) else {
            return Ok(None);
        };
        if stored.user != user {
            return Ok(None);
        }

        let validated = Playlist::new(
            playlist_id.clone(),
            name.to_owned(),
            comment.map(str::to_owned),
            public,
            stored.created_at,
            chrono::Utc::now(),
            count,
            duration,
        )
        .map_err(|_| TrackRepositoryError::InvalidData)?;

        stored.name = validated.name().to_owned();
        stored.comment = validated.comment().map(str::to_owned);
        stored.public = public;
        stored.updated_at = validated.updated_at();

        Ok(Some(validated))
    }

    async fn delete_playlist(
        &self,
        user: Uuid,
        playlist_id: &PlaylistId,
    ) -> Result<bool, TrackRepositoryError> {
        let mut indexes = locked(self)?;

        if !indexes
            .playlists
            .get(playlist_id)
            .is_some_and(|stored| stored.user == user)
        {
            return Ok(false);
        }

        indexes.playlists.remove(playlist_id);
        indexes.playlist_tracks.remove(playlist_id);

        Ok(true)
    }

    async fn find_playlist(
        &self,
        user: Uuid,
        playlist_id: &PlaylistId,
    ) -> Result<Option<Playlist>, TrackRepositoryError> {
        let indexes = locked(self)?;
        let Some(stored) = indexes.playlists.get(playlist_id) else {
            return Ok(None);
        };
        if stored.user != user {
            return Ok(None);
        }

        Ok(Some(stored.playlist(
            playlist_id,
            track_count(&indexes, playlist_id),
            duration_ms(&indexes, playlist_id),
        )))
    }

    async fn list_playlists(&self, user: Uuid) -> Result<Vec<Playlist>, TrackRepositoryError> {
        let indexes = locked(self)?;

        let mut playlists = indexes
            .playlists
            .iter()
            .filter(|(_, stored)| stored.user == user)
            .map(|(id, stored)| {
                stored.playlist(id, track_count(&indexes, id), duration_ms(&indexes, id))
            })
            .collect::<Vec<_>>();
        playlists.sort_by(|left, right| {
            left.name()
                .to_lowercase()
                .cmp(&right.name().to_lowercase())
                .then_with(|| left.id().to_string().cmp(&right.id().to_string()))
        });

        Ok(playlists)
    }

    async fn playlist_tracks(
        &self,
        playlist_id: &PlaylistId,
    ) -> Result<Vec<TrackId>, TrackRepositoryError> {
        let indexes = locked(self)?;

        Ok(indexes
            .playlist_tracks
            .get(playlist_id)
            .cloned()
            .unwrap_or_default())
    }

    async fn add_playlist_track(
        &self,
        user: Uuid,
        playlist_id: &PlaylistId,
        track_id: &TrackId,
    ) -> Result<bool, TrackRepositoryError> {
        let mut indexes = locked(self)?;

        if !indexes
            .playlists
            .get(playlist_id)
            .is_some_and(|stored| stored.user == user)
            || !indexes.by_track_id.contains_key(track_id)
        {
            return Ok(false);
        }

        let tracks = indexes
            .playlist_tracks
            .entry(playlist_id.clone())
            .or_default();
        if tracks.contains(track_id) {
            return Ok(false);
        }
        tracks.push(track_id.clone());

        Ok(true)
    }

    async fn remove_playlist_track(
        &self,
        user: Uuid,
        playlist_id: &PlaylistId,
        track_id: &TrackId,
    ) -> Result<bool, TrackRepositoryError> {
        let mut indexes = locked(self)?;

        if !indexes
            .playlists
            .get(playlist_id)
            .is_some_and(|stored| stored.user == user)
        {
            return Ok(false);
        }

        let Some(tracks) = indexes.playlist_tracks.get_mut(playlist_id) else {
            return Ok(false);
        };
        let before = tracks.len();
        tracks.retain(|existing| existing != track_id);

        Ok(tracks.len() != before)
    }
}
