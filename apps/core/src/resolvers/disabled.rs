use async_trait::async_trait;

use crate::{
    catalogue::{CatalogueCandidate, CatalogueResolver, CatalogueSearch},
    playback::{PlayableMedia, PlaybackResolver},
    request::RequestContext,
    tracks::SourceIdentity,
};

use super::errors::ResolverError;

#[derive(Debug, Default)]
pub struct DisabledResolver;

#[async_trait]
impl CatalogueResolver for DisabledResolver {
    async fn search(
        &self,
        _search: &CatalogueSearch,
        _context: &RequestContext,
    ) -> Result<Vec<CatalogueCandidate>, ResolverError> {
        Err(ResolverError::Disabled)
    }
}

#[async_trait]
impl PlaybackResolver for DisabledResolver {
    async fn resolve(
        &self,
        _source: &SourceIdentity,
        _context: &RequestContext,
    ) -> Result<PlayableMedia, ResolverError> {
        Err(ResolverError::Disabled)
    }
}
