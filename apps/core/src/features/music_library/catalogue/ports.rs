use super::domain::{CatalogueCandidate, CataloguePage, CatalogueSearch, FetchedAlbum};
use crate::{
    features::music_library::catalogue::ProviderError,
    features::music_library::domain::{AlbumMetadata, ArtistCredit, SourceIdentity},
    platform::request::RequestContext,
};
use async_trait::async_trait;

#[async_trait]
pub trait CatalogueProvider: Send + Sync {
    async fn search_tracks(
        &self,
        search: &CatalogueSearch,
        context: &RequestContext,
    ) -> Result<Vec<CatalogueCandidate>, ProviderError>;

    async fn search_albums(
        &self,
        _search: &CatalogueSearch,
        _context: &RequestContext,
    ) -> Result<CataloguePage<AlbumMetadata>, ProviderError> {
        Err(ProviderError::UnsupportedProvider)
    }

    async fn search_artists(
        &self,
        _search: &CatalogueSearch,
        _context: &RequestContext,
    ) -> Result<CataloguePage<ArtistCredit>, ProviderError> {
        Err(ProviderError::UnsupportedProvider)
    }

    async fn fetch_artist_albums(
        &self,
        _source: &SourceIdentity,
        _context: &RequestContext,
    ) -> Result<Vec<AlbumMetadata>, ProviderError> {
        Err(ProviderError::UnsupportedProvider)
    }

    async fn fetch_album(
        &self,
        _source: &SourceIdentity,
        _context: &RequestContext,
    ) -> Result<FetchedAlbum, ProviderError> {
        Err(ProviderError::UnsupportedProvider)
    }
}
