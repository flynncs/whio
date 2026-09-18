use async_trait::async_trait;

use crate::tracks::{ArtworkRef, Track, TrackMetadata};

use super::{ProviderId, SourceIdentity, SourceScope, TrackId};

#[derive(Debug, thiserror::Error)]
pub enum TrackRepositoryError {
    #[error("track repository is unavailable")]
    Unavailable,
    #[error("track repository contained invalid data")]
    InvalidData,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceFileFacts {
    pub path: Option<String>,
    pub size_bytes: u64,
    pub modified_at: Option<chrono::DateTime<chrono::Utc>>,
    pub missing_at: Option<chrono::DateTime<chrono::Utc>>,
    pub content_type: Option<String>,
    pub suffix: Option<String>,
}

#[async_trait]
pub trait TrackRepository: Send + Sync {
    async fn get_or_create(
        &self,
        source: SourceIdentity,
        metadata: TrackMetadata,
    ) -> Result<Track, TrackRepositoryError>;

    async fn find_sources(
        &self,
        track_id: &TrackId,
    ) -> Result<Vec<SourceIdentity>, TrackRepositoryError>;

    async fn find_track(&self, track_id: &TrackId) -> Result<Option<Track>, TrackRepositoryError>;

    /// record where one source keeps its cover art. repositories without
    /// artwork storage accept and discard it.
    async fn set_source_artwork(
        &self,
        _source: &SourceIdentity,
        _artwork: &ArtworkRef,
    ) -> Result<(), TrackRepositoryError> {
        Ok(())
    }

    /// the highest-precedence artwork among a track's sources, with the source
    /// that owns it so callers know how to resolve it.
    async fn find_artwork(
        &self,
        _track_id: &TrackId,
    ) -> Result<Option<(SourceIdentity, ArtworkRef)>, TrackRepositoryError> {
        Ok(None)
    }

    async fn set_source_file_facts(
        &self,
        _source: &SourceIdentity,
        _facts: &SourceFileFacts,
    ) -> Result<(), TrackRepositoryError> {
        Ok(())
    }

    async fn find_source_file_facts(
        &self,
        _source: &SourceIdentity,
    ) -> Result<Option<SourceFileFacts>, TrackRepositoryError> {
        Ok(None)
    }

    async fn list_scanned_paths(
        &self,
        _provider_id: &ProviderId,
        _source_scope: &SourceScope,
    ) -> Result<Vec<(SourceIdentity, SourceFileFacts)>, TrackRepositoryError> {
        Ok(Vec::new())
    }
}
