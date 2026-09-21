use async_trait::async_trait;
use uuid::Uuid;

use crate::tracks::{
    Album, AlbumEntry, AlbumId, AlbumListFilter, AlbumOrder, AlbumSummary, Artist, ArtistId,
    ArtistSummary, GenreSummary, SourceIdentity, Track, TrackHit, TrackId,
};

use super::TrackRepositoryError;

#[async_trait]
pub trait LibraryRead: Send + Sync {
    async fn find_track(&self, track_id: &TrackId) -> Result<Option<Track>, TrackRepositoryError>;

    async fn find_album(&self, _album_id: &AlbumId) -> Result<Option<Album>, TrackRepositoryError> {
        Ok(None)
    }

    async fn find_album_for_track(
        &self,
        _track_id: &TrackId,
    ) -> Result<Option<Album>, TrackRepositoryError> {
        Ok(None)
    }

    async fn find_album_summary(
        &self,
        _album_id: &AlbumId,
        _user: Option<Uuid>,
    ) -> Result<Option<AlbumSummary>, TrackRepositoryError> {
        Ok(None)
    }

    async fn find_album_entries(
        &self,
        _album_id: &AlbumId,
        _user: Option<Uuid>,
    ) -> Result<Vec<AlbumEntry>, TrackRepositoryError> {
        Ok(Vec::new())
    }

    async fn find_albums_for_artist(
        &self,
        _artist_id: &ArtistId,
        _user: Option<Uuid>,
    ) -> Result<Vec<AlbumSummary>, TrackRepositoryError> {
        Ok(Vec::new())
    }

    async fn find_artist(
        &self,
        _artist_id: &ArtistId,
        _user: Option<Uuid>,
    ) -> Result<Option<Artist>, TrackRepositoryError> {
        Ok(None)
    }

    async fn list_albums(
        &self,
        _offset: u32,
        _limit: u32,
        _order: AlbumOrder,
        _filter: AlbumListFilter,
        _user: Option<Uuid>,
    ) -> Result<Vec<AlbumSummary>, TrackRepositoryError> {
        Ok(Vec::new())
    }

    async fn list_artists(
        &self,
        _offset: u32,
        _limit: u32,
        _user: Option<Uuid>,
    ) -> Result<Vec<ArtistSummary>, TrackRepositoryError> {
        Ok(Vec::new())
    }

    async fn count_albums(
        &self,
        _user: Option<Uuid>,
        _filter: AlbumListFilter,
    ) -> Result<u32, TrackRepositoryError> {
        Ok(0)
    }

    async fn count_artists(&self, _user: Option<Uuid>) -> Result<u32, TrackRepositoryError> {
        Ok(0)
    }

    async fn search_tracks(
        &self,
        _query: &str,
        _limit: u32,
        _user: Option<Uuid>,
    ) -> Result<Vec<TrackHit>, TrackRepositoryError> {
        Ok(Vec::new())
    }

    async fn search_albums(
        &self,
        _query: &str,
        _limit: u32,
        _user: Option<Uuid>,
    ) -> Result<Vec<AlbumSummary>, TrackRepositoryError> {
        Ok(Vec::new())
    }

    async fn search_artists(
        &self,
        _query: &str,
        _limit: u32,
        _user: Option<Uuid>,
    ) -> Result<Vec<ArtistSummary>, TrackRepositoryError> {
        Ok(Vec::new())
    }

    async fn candidate_tracks(
        &self,
        _titles: &[String],
        _limit: u32,
    ) -> Result<Vec<TrackHit>, TrackRepositoryError> {
        Ok(Vec::new())
    }

    async fn dedup_candidates(
        &self,
        _keys: &[String],
        _limit: u32,
    ) -> Result<Vec<TrackHit>, TrackRepositoryError> {
        Ok(Vec::new())
    }

    async fn list_genres(&self) -> Result<Vec<GenreSummary>, TrackRepositoryError> {
        Ok(Vec::new())
    }
}

#[async_trait]
pub trait SourceLookup: Send + Sync {
    async fn find_sources(
        &self,
        track_id: &TrackId,
    ) -> Result<Vec<SourceIdentity>, TrackRepositoryError>;

    async fn find_album_sources(
        &self,
        _album_id: &AlbumId,
    ) -> Result<Vec<SourceIdentity>, TrackRepositoryError> {
        Ok(Vec::new())
    }

    async fn find_artist_sources(
        &self,
        _artist_id: &ArtistId,
    ) -> Result<Vec<SourceIdentity>, TrackRepositoryError> {
        Ok(Vec::new())
    }

    async fn set_source_artwork(
        &self,
        _source: &SourceIdentity,
        _artwork: &crate::tracks::ArtworkRef,
    ) -> Result<(), TrackRepositoryError> {
        Ok(())
    }

    async fn find_artwork(
        &self,
        _track_id: &TrackId,
    ) -> Result<Option<(SourceIdentity, crate::tracks::ArtworkRef)>, TrackRepositoryError> {
        Ok(None)
    }

    async fn find_source_file_facts(
        &self,
        _source: &SourceIdentity,
    ) -> Result<Option<super::SourceFileFacts>, TrackRepositoryError> {
        Ok(None)
    }

    async fn list_scanned_paths(
        &self,
        _provider_id: &crate::tracks::ProviderId,
        _source_scope: &crate::tracks::SourceScope,
    ) -> Result<Vec<(SourceIdentity, super::SourceFileFacts)>, TrackRepositoryError> {
        Ok(Vec::new())
    }
}
