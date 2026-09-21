use async_trait::async_trait;

use crate::tracks::CatalogueWrite;

use super::InMemoryTrackRepository;
use super::*;

#[async_trait]
impl CatalogueWrite for InMemoryTrackRepository {
    async fn get_or_create(
        &self,
        source: SourceIdentity,
        metadata: TrackMetadata,
    ) -> Result<Track, TrackRepositoryError> {
        let mut indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        let track_id = match identified_track(&indexes, &metadata) {
            Some(identified) => {
                if let Some(previous) = indexes.by_source.get(&source).cloned()
                    && previous != identified
                {
                    detach_source(&mut indexes, &source, &previous);
                    drop_source_less_track(&mut indexes, &previous);
                }

                attach_source(&mut indexes, &source, &identified);

                identified
            }
            None => match indexes.by_source.get(&source).cloned() {
                Some(current) => current,
                None => {
                    let track_id = TrackId::new();
                    let track = Track::new(track_id.clone(), metadata.clone());

                    indexes.by_track_id.insert(
                        track_id.clone(),
                        TrackEntry {
                            track,
                            sources: vec![source.clone()],
                        },
                    );
                    indexes.by_source.insert(source, track_id.clone());

                    track_id
                }
            },
        };

        let entry = indexes
            .by_track_id
            .get_mut(&track_id)
            .ok_or(TrackRepositoryError::Unavailable)?;

        entry.track = Track::new(track_id, metadata);

        Ok(entry.track.clone())
    }

    async fn resolve_or_create_artist(
        &self,
        credit: &ArtistCredit,
    ) -> Result<Artist, TrackRepositoryError> {
        let mut indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        Ok(resolve_artist(&mut indexes, credit))
    }

    async fn resolve_or_create_album(
        &self,
        metadata: &AlbumMetadata,
    ) -> Result<Album, TrackRepositoryError> {
        let mut indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        let album_artist_id = metadata
            .album_artist()
            .map(|credit| resolve_artist(&mut indexes, credit).id().clone());

        Ok(resolve_album(&mut indexes, metadata, album_artist_id))
    }

    async fn set_album_source(
        &self,
        album_id: &AlbumId,
        source: &SourceIdentity,
    ) -> Result<(), TrackRepositoryError> {
        let mut indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        if !indexes.albums_by_id.contains_key(album_id) {
            return Err(TrackRepositoryError::InvalidData);
        }

        let sources = indexes.album_sources.entry(album_id.clone()).or_default();
        if !sources.contains(source) {
            sources.push(source.clone());
        }

        Ok(())
    }

    async fn set_artist_source(
        &self,
        artist_id: &ArtistId,
        source: &SourceIdentity,
    ) -> Result<(), TrackRepositoryError> {
        let mut indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        if !indexes.artists_by_id.contains_key(artist_id) {
            return Err(TrackRepositoryError::InvalidData);
        }

        let sources = indexes.artist_sources.entry(artist_id.clone()).or_default();
        if !sources.contains(source) {
            sources.push(source.clone());
        }

        Ok(())
    }

    async fn set_album_track(
        &self,
        album_id: &AlbumId,
        track_id: &TrackId,
        membership: &AlbumTrack,
    ) -> Result<(), TrackRepositoryError> {
        let mut indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        if !indexes.albums_by_id.contains_key(album_id)
            || !indexes.by_track_id.contains_key(track_id)
        {
            return Err(TrackRepositoryError::InvalidData);
        }

        detach_track(&mut indexes.album_tracks, Some(album_id), track_id);

        let tracks = indexes.album_tracks.entry(album_id.clone()).or_default();
        match tracks.iter_mut().find(|(id, _)| id == track_id) {
            Some(entry) => entry.1 = *membership,
            None => tracks.push((track_id.clone(), *membership)),
        }

        Ok(())
    }

    async fn clear_album_track(&self, track_id: &TrackId) -> Result<(), TrackRepositoryError> {
        let mut indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        detach_track(&mut indexes.album_tracks, None, track_id);
        Ok(())
    }

    async fn set_source_file_facts(
        &self,
        source: &SourceIdentity,
        facts: &SourceFileFacts,
    ) -> Result<(), TrackRepositoryError> {
        let mut indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        indexes
            .file_facts_by_source
            .insert(source.clone(), facts.clone());
        Ok(())
    }
}
