use async_trait::async_trait;

use crate::tracks::{
    Album, AlbumId, AlbumMetadata, AlbumTrack, Artist, ArtistCredit, ArtistId, SourceIdentity,
    Track, TrackId, TrackMetadata,
};

use super::{SourceFileFacts, TrackRepositoryError};

#[async_trait]
pub trait CatalogueWrite: Send + Sync {
    async fn get_or_create(
        &self,
        source: SourceIdentity,
        metadata: TrackMetadata,
    ) -> Result<Track, TrackRepositoryError>;

    async fn set_source_file_facts(
        &self,
        _source: &SourceIdentity,
        _facts: &SourceFileFacts,
    ) -> Result<(), TrackRepositoryError> {
        Ok(())
    }

    async fn index_source(
        &self,
        source: SourceIdentity,
        metadata: TrackMetadata,
        facts: &SourceFileFacts,
    ) -> Result<Track, TrackRepositoryError> {
        let track = self.get_or_create(source.clone(), metadata).await?;
        self.set_source_file_facts(&source, facts).await?;

        Ok(track)
    }

    async fn resolve_or_create_artist(
        &self,
        _credit: &ArtistCredit,
    ) -> Result<Artist, TrackRepositoryError> {
        Err(TrackRepositoryError::Unavailable)
    }

    async fn resolve_or_create_album(
        &self,
        _metadata: &AlbumMetadata,
    ) -> Result<Album, TrackRepositoryError> {
        Err(TrackRepositoryError::Unavailable)
    }

    async fn set_album_source(
        &self,
        _album_id: &AlbumId,
        _source: &SourceIdentity,
    ) -> Result<(), TrackRepositoryError> {
        Ok(())
    }

    async fn set_artist_source(
        &self,
        _artist_id: &ArtistId,
        _source: &SourceIdentity,
    ) -> Result<(), TrackRepositoryError> {
        Ok(())
    }

    async fn set_album_track(
        &self,
        _album_id: &AlbumId,
        _track_id: &TrackId,
        _membership: &AlbumTrack,
    ) -> Result<(), TrackRepositoryError> {
        Ok(())
    }

    async fn clear_album_track(&self, _track_id: &TrackId) -> Result<(), TrackRepositoryError> {
        Ok(())
    }
}
