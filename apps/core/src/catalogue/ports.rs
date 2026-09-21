use async_trait::async_trait;
use uuid::Uuid;

use super::domain::{CatalogueCandidate, CatalogueSearch};
use crate::{providers::ProviderError, request::RequestContext, tracks::SourceIdentity};

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
    ) -> Result<Vec<CatalogueCandidate>, ProviderError> {
        Err(ProviderError::UnsupportedProvider)
    }
}
