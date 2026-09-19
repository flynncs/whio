use async_trait::async_trait;

use crate::tracks::{ArtworkRef, Track, TrackMetadata};

use super::{
    Album, AlbumEntry, AlbumId, AlbumMetadata, AlbumSummary, AlbumTrack, Artist, ArtistCredit,
    ArtistId, ArtistSummary, ProviderId, SourceIdentity, SourceScope, TrackId,
};

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
    pub acoustid_fingerprint: Option<String>,
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

    /// the one artist ladder. a MusicBrainz id match wins; otherwise a
    /// normalized-name match among artists that carry no id, attaching the
    /// incoming id when the existing row has none; otherwise a new artist.
    async fn resolve_or_create_artist(
        &self,
        _credit: &ArtistCredit,
    ) -> Result<Artist, TrackRepositoryError> {
        Err(TrackRepositoryError::Unavailable)
    }

    /// the one album ladder (A-044). a release-group or release id match wins;
    /// otherwise a normalized (title, album artist) match where at most one
    /// side carries an identifier; otherwise a new album.
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

    async fn set_album_track(
        &self,
        _album_id: &AlbumId,
        _track_id: &TrackId,
        _membership: &AlbumTrack,
    ) -> Result<(), TrackRepositoryError> {
        Ok(())
    }

    /// removes a track's album membership without filing it elsewhere (A-048).
    async fn clear_album_track(&self, _track_id: &TrackId) -> Result<(), TrackRepositoryError> {
        Ok(())
    }

    async fn find_album(&self, _album_id: &AlbumId) -> Result<Option<Album>, TrackRepositoryError> {
        Ok(None)
    }

    async fn find_album_for_track(
        &self,
        _track_id: &TrackId,
    ) -> Result<Option<Album>, TrackRepositoryError> {
        Ok(None)
    }

    async fn find_artist(
        &self,
        _artist_id: &ArtistId,
    ) -> Result<Option<Artist>, TrackRepositoryError> {
        Ok(None)
    }

    async fn find_album_sources(
        &self,
        _album_id: &AlbumId,
    ) -> Result<Vec<SourceIdentity>, TrackRepositoryError> {
        Ok(Vec::new())
    }

    async fn find_album_entries(
        &self,
        _album_id: &AlbumId,
    ) -> Result<Vec<AlbumEntry>, TrackRepositoryError> {
        Ok(Vec::new())
    }

    async fn find_albums_for_artist(
        &self,
        _artist_id: &ArtistId,
    ) -> Result<Vec<AlbumSummary>, TrackRepositoryError> {
        Ok(Vec::new())
    }

    async fn list_albums(
        &self,
        _offset: u32,
        _limit: u32,
    ) -> Result<Vec<AlbumSummary>, TrackRepositoryError> {
        Ok(Vec::new())
    }

    async fn list_artists(
        &self,
        _offset: u32,
        _limit: u32,
    ) -> Result<Vec<ArtistSummary>, TrackRepositoryError> {
        Ok(Vec::new())
    }
}
