use async_trait::async_trait;

use crate::features::music_library::domain::{
    Album, AlbumId, AlbumMetadata, AlbumTrack, Artist, ArtistCredit, ArtistId, ProviderEntityState,
    ProviderId, SourceIdentity, SourceScope, Track, TrackId, TrackMetadata,
};

use super::{SourceFileFacts, TrackRepositoryError};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReconcileReport {
    pub marked_missing: usize,
    pub reappeared: usize,
}

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

    async fn reconcile_missing(
        &self,
        _provider: &ProviderId,
        _scope: &SourceScope,
        _seen: &[String],
        _now: chrono::DateTime<chrono::Utc>,
    ) -> Result<ReconcileReport, TrackRepositoryError> {
        Err(TrackRepositoryError::Unavailable)
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

    async fn set_provider_entity_state(
        &self,
        _source: &SourceIdentity,
        _state: &ProviderEntityState,
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
