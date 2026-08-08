use async_trait::async_trait;

use super::domain::{CatalogueCandidate, CatalogueSearch};
use crate::{request::RequestContext, resolvers::ResolverError};

#[async_trait]
pub trait CatalogueResolver: Send + Sync {
    async fn search(
        &self,
        search: &CatalogueSearch,
        context: &RequestContext,
    ) -> Result<Vec<CatalogueCandidate>, ResolverError>;
}
