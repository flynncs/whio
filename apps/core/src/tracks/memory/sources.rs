use async_trait::async_trait;

use crate::tracks::SourceLookup;

use super::InMemoryTrackRepository;
use super::*;

#[async_trait]
impl SourceLookup for InMemoryTrackRepository {
    async fn find_sources(
        &self,
        track_id: &TrackId,
    ) -> Result<Vec<SourceIdentity>, TrackRepositoryError> {
        let indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        Ok(indexes
            .by_track_id
            .get(track_id)
            .map(|entry| entry.sources.clone())
            .unwrap_or_default())
    }

    async fn find_album_sources(
        &self,
        album_id: &AlbumId,
    ) -> Result<Vec<SourceIdentity>, TrackRepositoryError> {
        let indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        Ok(indexes
            .album_sources
            .get(album_id)
            .cloned()
            .unwrap_or_default())
    }

    async fn find_artist_sources(
        &self,
        artist_id: &ArtistId,
    ) -> Result<Vec<SourceIdentity>, TrackRepositoryError> {
        let indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        Ok(indexes
            .artist_sources
            .get(artist_id)
            .cloned()
            .unwrap_or_default())
    }

    async fn find_provider_entity_state(
        &self,
        source: &SourceIdentity,
    ) -> Result<Option<ProviderEntityState>, TrackRepositoryError> {
        let indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        Ok(indexes.provider_entity_state.get(source).cloned())
    }

    async fn set_source_artwork(
        &self,
        source: &SourceIdentity,
        artwork: &ArtworkRef,
    ) -> Result<(), TrackRepositoryError> {
        let mut indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        indexes
            .artwork_by_source
            .insert(source.clone(), artwork.clone());
        Ok(())
    }

    async fn find_artwork(
        &self,
        track_id: &TrackId,
    ) -> Result<Option<(SourceIdentity, ArtworkRef)>, TrackRepositoryError> {
        let indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        let Some(entry) = indexes.by_track_id.get(track_id) else {
            return Ok(None);
        };

        Ok(entry
            .sources
            .iter()
            .filter_map(|source| {
                indexes
                    .artwork_by_source
                    .get(source)
                    .map(|artwork| (source, artwork))
            })
            .min_by_key(|(source, _)| source.provider_id().precedence())
            .map(|(source, artwork)| (source.clone(), artwork.clone())))
    }

    async fn find_source_file_facts(
        &self,
        source: &SourceIdentity,
    ) -> Result<Option<SourceFileFacts>, TrackRepositoryError> {
        let indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        Ok(indexes.file_facts_by_source.get(source).cloned())
    }

    async fn list_scanned_paths(
        &self,
        provider_id: &ProviderId,
        source_scope: &SourceScope,
    ) -> Result<Vec<(SourceIdentity, SourceFileFacts)>, TrackRepositoryError> {
        let indexes = self
            .indexes
            .lock()
            .map_err(|_| TrackRepositoryError::Unavailable)?;

        Ok(indexes
            .file_facts_by_source
            .iter()
            .filter(|(source, _)| {
                source.provider_id() == provider_id && source.source_scope() == source_scope
            })
            .map(|(source, facts)| (source.clone(), facts.clone()))
            .collect())
    }
}
