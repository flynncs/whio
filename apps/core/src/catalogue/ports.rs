use async_trait::async_trait;

use super::domain::CatalogueCandidate;
use crate::resolvers::ResolverError;

#[async_trait]
pub trait CatalogueResolver: Send + Sync {
    async fn search(
        &self,
        query: &str,
        limit: u32,
        request_id: Option<&str>,
    ) -> Result<Vec<CatalogueCandidate>, ResolverError>;
}
