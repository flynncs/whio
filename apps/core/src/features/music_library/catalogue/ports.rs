use async_trait::async_trait;
use uuid::Uuid;

use super::domain::{CatalogueCandidate, CatalogueSearch, FetchedAlbum};
use crate::{
    features::music_library::catalogue::ProviderError,
    features::music_library::domain::SourceIdentity, platform::request::RequestContext,
};

#[async_trait]
pub trait CatalogueProvider: Send + Sync {
    async fn search(
        &self,
        search: &CatalogueSearch,
        user: Option<&Uuid>,
        context: &RequestContext,
    ) -> Result<Vec<CatalogueCandidate>, ProviderError>;

    async fn fetch_album(
        &self,
        _source: &SourceIdentity,
        _context: &RequestContext,
    ) -> Result<FetchedAlbum, ProviderError> {
        Err(ProviderError::UnsupportedProvider)
    }
}
